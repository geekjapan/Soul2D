//! Depth editing viewport and "depth draw" composer, as pure functions over plain data.
//!
//! Port of D `viewport/depth` (camera, common/targetview, draw/*, tools/*) and the `io/depthsample`
//! and `io/depthimage` pieces they call. Nothing here touches a document or a session: the caller
//! hands in pixels, vertices and settings and gets numbers back, so every function can be tested
//! against the D contract with literal values (`tests/depth_draw.rs`).
//!
//! What "depth draw" is in D: it composes the depth of PSD/PNG depth *layers* onto the vertices of
//! a depth-mapped grid (`compose_target`), with merge policies, per-layer z transforms and cleanup.
//! It is not a freehand brush. The four depth tools (direct depth, ring, attached point, plane)
//! only create the same `depth-ops` entries the session already stores; their gesture maths lives in
//! the "tool maths" section below.
//!
//! Coordinates follow D `float` (f32) throughout so a composed depth matches D to the last bit that
//! f32 arithmetic allows. The grid-to-document step is left to the caller (`document_points`).
//!
//! The sampling code duplicates the private copy in `psd_depth.rs` (same D source); the two are
//! independent so neither module needs the other.

use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

// ------------------------------------------------------------------------------------- sampling

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    AverageRgb,
    R,
    G,
    B,
    Luminance,
}

impl Channel {
    pub const ALL: [Channel; 5] = [Channel::AverageRgb, Channel::R, Channel::G, Channel::B, Channel::Luminance];

    pub fn name(self) -> &'static str {
        match self {
            Channel::AverageRgb => "AverageRGB",
            Channel::R => "R",
            Channel::G => "G",
            Channel::B => "B",
            Channel::Luminance => "Luminance",
        }
    }

    pub fn from_name(name: &str) -> Result<Self, String> {
        Ok(match name {
            "AverageRGB" => Channel::AverageRgb,
            "R" => Channel::R,
            "G" => Channel::G,
            "B" => Channel::B,
            "Luminance" => Channel::Luminance,
            other => return Err(format!("Unknown depth image channel: {other}")),
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Convolution {
    Nearest,
    Box3x3,
    Box5x5,
    Gaussian3x3,
    Gaussian5x5,
    Median3x3,
    Frontmost3x3,
    Backmost3x3,
    BoxCustom,
    GaussianCustom,
    MedianCustom,
    FrontmostCustom,
    BackmostCustom,
}

const CONVOLUTION_NAMES: [(Convolution, &str); 13] = [
    (Convolution::Nearest, "Nearest"),
    (Convolution::Box3x3, "Box3x3"),
    (Convolution::Box5x5, "Box5x5"),
    (Convolution::Gaussian3x3, "Gaussian3x3"),
    (Convolution::Gaussian5x5, "Gaussian5x5"),
    (Convolution::Median3x3, "Median3x3"),
    (Convolution::Frontmost3x3, "Frontmost3x3"),
    (Convolution::Backmost3x3, "Backmost3x3"),
    (Convolution::BoxCustom, "BoxCustom"),
    (Convolution::GaussianCustom, "GaussianCustom"),
    (Convolution::MedianCustom, "MedianCustom"),
    (Convolution::FrontmostCustom, "FrontmostCustom"),
    (Convolution::BackmostCustom, "BackmostCustom"),
];

impl Convolution {
    /// Every convolution, in D's declaration order.
    pub fn all() -> impl Iterator<Item = Convolution> {
        CONVOLUTION_NAMES.iter().map(|(c, _)| *c)
    }

    pub fn name(self) -> &'static str {
        CONVOLUTION_NAMES.iter().find(|(c, _)| *c == self).map_or("Nearest", |(_, n)| n)
    }

    pub fn from_name(name: &str) -> Result<Self, String> {
        CONVOLUTION_NAMES
            .iter()
            .find(|(_, n)| *n == name)
            .map(|(c, _)| *c)
            .ok_or_else(|| format!("Unknown depth image convolution: {name}"))
    }

    fn radius(self, custom_radius: i32) -> i32 {
        use Convolution::*;
        match self {
            Nearest => 0,
            Box3x3 | Gaussian3x3 | Median3x3 | Frontmost3x3 | Backmost3x3 => 1,
            Box5x5 | Gaussian5x5 => 2,
            BoxCustom | GaussianCustom | MedianCustom | FrontmostCustom | BackmostCustom => custom_radius.max(1),
        }
    }

    fn kernel_weight(self, custom_radius: i32, dx: i32, dy: i32) -> f32 {
        use Convolution::*;
        match self {
            Gaussian3x3 => ((2 - dx.abs()) * (2 - dy.abs())) as f32,
            Gaussian5x5 => (pascal5(dx) * pascal5(dy)) as f32,
            GaussianCustom => {
                let radius = if custom_radius > 0 { custom_radius } else { 1 };
                let sigma = (radius as f32 / 2.0).max(1.0);
                let distance2 = (dx * dx + dy * dy) as f32;
                (-distance2 / (2.0 * sigma * sigma)).exp()
            }
            _ => 1.0,
        }
    }

    fn weighted(self) -> bool {
        use Convolution::*;
        matches!(self, Box3x3 | Box5x5 | Gaussian3x3 | Gaussian5x5 | BoxCustom | GaussianCustom)
    }

    fn median(self) -> bool {
        matches!(self, Convolution::Median3x3 | Convolution::MedianCustom)
    }

    fn frontmost(self) -> bool {
        matches!(self, Convolution::Frontmost3x3 | Convolution::FrontmostCustom)
    }

    fn backmost(self) -> bool {
        matches!(self, Convolution::Backmost3x3 | Convolution::BackmostCustom)
    }
}

fn pascal5(d: i32) -> i32 {
    match d.abs() {
        0 => 6,
        1 => 4,
        2 => 1,
        _ => 0,
    }
}

pub const MIN_CUSTOM_RADIUS: i32 = 1;
pub const MAX_CUSTOM_RADIUS: i32 = 64;

pub fn normalize_custom_radius(radius: i32) -> i32 {
    radius.clamp(MIN_CUSTOM_RADIUS, MAX_CUSTOM_RADIUS)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleSettings {
    pub invert: bool,
    pub back_depth: f32,
    pub front_depth: f32,
    pub depth_scale: f32,
    pub alpha_threshold: f32,
    pub custom_radius: i32,
    pub convolution: Convolution,
    pub channel: Channel,
}

impl Default for SampleSettings {
    fn default() -> Self {
        Self {
            invert: false,
            back_depth: -1.0,
            front_depth: 1.0,
            depth_scale: 1.0,
            alpha_threshold: 0.01,
            custom_radius: 3,
            convolution: Convolution::Gaussian3x3,
            channel: Channel::AverageRgb,
        }
    }
}

/// Result of one depth sample: `valid` is false when every pixel in the window was missing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Sample {
    pub valid: bool,
    pub value: f32,
}

#[derive(Clone, Copy)]
struct Point {
    valid: bool,
    value: f32,
    weight: f32,
}

const MISSING: Point = Point { valid: false, value: 0.0, weight: 0.0 };

/// The normal-image alpha that gates which depth pixels count (D `normalCoverage`).
#[derive(Clone, Copy)]
pub struct Coverage<'a> {
    pub rgba: &'a [u8],
    pub width: i32,
    pub height: i32,
    pub opacity: f32,
}

fn clamp01(value: f32) -> f32 {
    value.max(0.0).min(1.0)
}

fn pixel_depth01(data: &[u8], index: usize, channel: Channel, invert: bool) -> f32 {
    let (r, g, b) = (f32::from(data[index]), f32::from(data[index + 1]), f32::from(data[index + 2]));
    let depth01 = match channel {
        Channel::AverageRgb => (r + g + b) / (255.0 * 3.0),
        Channel::R => r / 255.0,
        Channel::G => g / 255.0,
        Channel::B => b / 255.0,
        Channel::Luminance => (0.2126 * r + 0.7152 * g + 0.0722 * b) / 255.0,
    };
    if invert {
        1.0 - depth01
    } else {
        depth01
    }
}

fn coverage_alpha_at(x: i32, y: i32, width: i32, height: i32, coverage: Option<&Coverage>) -> f32 {
    let Some(c) = coverage else { return 1.0 };
    if c.rgba.is_empty() || c.width <= 0 || c.height <= 0 {
        return 1.0;
    }
    if x < 0 || y < 0 || x >= width || y >= height {
        return 0.0;
    }
    let cx = ((((x as f32 + 0.5) * c.width as f32 / width as f32) - 0.5).round() as i32).clamp(0, c.width - 1);
    let cy = ((((y as f32 + 0.5) * c.height as f32 / height as f32) - 0.5).round() as i32).clamp(0, c.height - 1);
    let index = (cy as usize * c.width as usize + cx as usize) * 4;
    match c.rgba.get(index + 3) {
        Some(alpha) => f32::from(*alpha) / 255.0,
        None => 1.0,
    }
}

/// D `ngDepthImageSampleRgba*`: the depth of the layer pixel window around `(x, y)`.
///
/// `coverage` is the optional normal-image alpha; its `opacity` multiplies the accepted alpha.
pub fn sample_rgba(
    rgba: &[u8],
    width: i32,
    height: i32,
    coverage: Option<Coverage>,
    layer_opacity: f32,
    x: f32,
    y: f32,
    settings: &SampleSettings,
) -> Result<Sample, String> {
    if width < 0 || height < 0 {
        return Err("Image dimensions must be non-negative".into());
    }
    if rgba.len() < (width as usize).saturating_mul(height as usize).saturating_mul(4) {
        return Err("RGBA buffer is smaller than dimensions".into());
    }
    if let Some(c) = &coverage {
        if c.width < 0 || c.height < 0 {
            return Err("Coverage dimensions must be non-negative".into());
        }
        if c.rgba.len() < (c.width as usize).saturating_mul(c.height as usize).saturating_mul(4) {
            return Err("Coverage RGBA buffer is smaller than dimensions".into());
        }
    }
    let opacity = clamp01(layer_opacity);
    let coverage_opacity = coverage.as_ref().map_or(1.0, |c| clamp01(c.opacity));
    let sample_at = |sx: i32, sy: i32| -> Point {
        if sx < 0 || sy < 0 || sx >= width || sy >= height {
            return MISSING;
        }
        let cov_alpha = coverage_alpha_at(sx, sy, width, height, coverage.as_ref());
        let reliable = coverage.as_ref().map_or(true, |c| c.rgba.is_empty()) || cov_alpha > 0.5;
        if !reliable {
            return MISSING;
        }
        let index = (sy as usize * width as usize + sx as usize) * 4;
        let alpha = (f32::from(rgba[index + 3]) / 255.0 * opacity) * cov_alpha * coverage_opacity;
        if alpha <= settings.alpha_threshold {
            return MISSING;
        }
        let depth01 = pixel_depth01(rgba, index, settings.channel, settings.invert);
        let depth = (settings.back_depth + (settings.front_depth - settings.back_depth) * depth01) * settings.depth_scale;
        Point { valid: true, value: depth, weight: alpha }
    };
    Ok(convolve(&sample_at, settings.convolution, settings.custom_radius, x.round() as i32, y.round() as i32))
}

fn convolve(sample_at: &dyn Fn(i32, i32) -> Point, convolution: Convolution, custom_radius: i32, cx: i32, cy: i32) -> Sample {
    let none = Sample { valid: false, value: 0.0 };
    let radius = convolution.radius(custom_radius);
    if convolution == Convolution::Nearest {
        let p = sample_at(cx, cy);
        return if p.valid { Sample { valid: true, value: p.value } } else { none };
    }
    let mut window: Vec<(i32, i32, Point)> = Vec::new();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let p = sample_at(cx + dx, cy + dy);
            if p.valid {
                window.push((dx, dy, p));
            }
        }
    }
    if convolution.weighted() {
        let (mut total, mut weight_total) = (0.0f32, 0.0f32);
        for (dx, dy, p) in &window {
            let weight = convolution.kernel_weight(custom_radius, *dx, *dy) * p.weight;
            if weight > 0.0 {
                total += p.value * weight;
                weight_total += weight;
            }
        }
        return if weight_total <= 0.0 { none } else { Sample { valid: true, value: total / weight_total } };
    }
    if convolution.median() {
        let mut values: Vec<f32> = window.iter().map(|(_, _, p)| p.value).collect();
        if values.is_empty() {
            return none;
        }
        values.sort_by(f32::total_cmp);
        return Sample { valid: true, value: values[values.len() / 2] };
    }
    if convolution.frontmost() || convolution.backmost() {
        let front = convolution.frontmost();
        let mut best: Option<f32> = None;
        for (_, _, p) in &window {
            if best.map_or(true, |b| if front { p.value > b } else { p.value < b }) {
                best = Some(p.value);
            }
        }
        return best.map_or(none, |value| Sample { valid: true, value });
    }
    none
}

// ----------------------------------------------------------------------------- layers, bindings

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergePolicy {
    Replace,
    Frontmost,
    Backmost,
    Add,
    KeepExistingWhereMissing,
}

impl MergePolicy {
    pub fn name(self) -> &'static str {
        match self {
            MergePolicy::Replace => "Replace",
            MergePolicy::Frontmost => "Frontmost",
            MergePolicy::Backmost => "Backmost",
            MergePolicy::Add => "Add",
            MergePolicy::KeepExistingWhereMissing => "KeepExistingWhereMissing",
        }
    }

    pub fn from_name(name: &str) -> Result<Self, String> {
        Ok(match name {
            "Replace" => MergePolicy::Replace,
            "Frontmost" => MergePolicy::Frontmost,
            "Backmost" => MergePolicy::Backmost,
            "Add" => MergePolicy::Add,
            "KeepExistingWhereMissing" => MergePolicy::KeepExistingWhereMissing,
            other => return Err(format!("Unknown DepthDraw merge policy: {other}")),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub layer_id: String,
    pub target_node_uuid: u64,
    pub target_grid_uuid: u64,
    pub order: i32,
    pub enabled: bool,
    pub use_normal_layer_alpha: bool,
    pub coverage_threshold: f32,
    pub merge_policy: MergePolicy,
}

impl Default for Binding {
    fn default() -> Self {
        Self {
            layer_id: String::new(),
            target_node_uuid: 0,
            target_grid_uuid: 0,
            order: 0,
            enabled: true,
            use_normal_layer_alpha: true,
            coverage_threshold: 0.01,
            merge_policy: MergePolicy::Frontmost,
        }
    }
}

impl Binding {
    pub fn applies_to(&self, grid_uuid: u64) -> bool {
        self.enabled && self.target_grid_uuid == grid_uuid && !self.layer_id.is_empty()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CleanupKind {
    AlphaDepthGapFill,
    ContourRepair,
}

pub const MAX_CONTOUR_THICKNESS: i32 = 64;
pub const MAX_FOCUSED_RULE_RADIUS: i32 = 64;

pub fn normalize_contour_thickness(thickness: i32) -> i32 {
    thickness.clamp(1, MAX_CONTOUR_THICKNESS)
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FocusedRule {
    pub layer_index: i32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub lift: i32,
    pub radius: i32,
}

/// D `ngNormalizeDepthDrawFocusedRule`: clamp a rule to the layer and to the allowed ranges.
pub fn normalize_focused_rule(mut rule: FocusedRule, width: i32, height: i32) -> FocusedRule {
    let width = width.max(0);
    let height = height.max(0);
    rule.x = rule.x.clamp(0, width);
    rule.y = rule.y.clamp(0, height);
    rule.w = rule.w.max(0).min(width - rule.x);
    rule.h = rule.h.max(0).min(height - rule.y);
    rule.lift = rule.lift.clamp(0, 255);
    rule.radius = rule.radius.clamp(0, MAX_FOCUSED_RULE_RADIUS);
    rule
}

#[derive(Clone, Debug, PartialEq)]
pub struct CleanupOperation {
    pub kind: CleanupKind,
    pub contour_thickness: i32,
    pub focused_rules: Vec<FocusedRule>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub id: String,
    pub source_path: String,
    pub layer_path: String,
    pub display_name: String,
    pub width: i32,
    pub height: i32,
    pub bounds: Rect,
    pub rgba: Vec<u8>,
    pub depth_pixels: Vec<u8>,
    pub alpha_mask: Vec<u8>,
    pub normal_coverage: Vec<u8>,
    pub cleanup_operations: Vec<CleanupOperation>,
    pub opacity: f32,
    pub visible: bool,
    pub enabled: bool,
    pub xy_offset: [f32; 2],
    pub xy_scale: [f32; 2],
    pub z_offset: f32,
    pub z_scale: f32,
    pub back_depth: f32,
    pub front_depth: f32,
    pub sample_depth_scale: f32,
    pub invert: bool,
    pub channel: Channel,
    pub convolution: Convolution,
    pub custom_radius: i32,
    pub alpha_threshold: f32,
}

impl Default for Layer {
    fn default() -> Self {
        Self {
            id: String::new(),
            source_path: String::new(),
            layer_path: String::new(),
            display_name: String::new(),
            width: 0,
            height: 0,
            bounds: Rect::default(),
            rgba: Vec::new(),
            depth_pixels: Vec::new(),
            alpha_mask: Vec::new(),
            normal_coverage: Vec::new(),
            cleanup_operations: Vec::new(),
            opacity: 1.0,
            visible: true,
            enabled: true,
            xy_offset: [0.0, 0.0],
            xy_scale: [1.0, 1.0],
            z_offset: 0.0,
            z_scale: 1.0,
            back_depth: -1.0,
            front_depth: 1.0,
            sample_depth_scale: 1.0,
            invert: false,
            channel: Channel::AverageRgb,
            convolution: Convolution::Gaussian3x3,
            custom_radius: 3,
            alpha_threshold: 0.01,
        }
    }
}

impl Layer {
    // Saturating: a manifest may claim any size for a layer that has no pixels.
    fn byte_count(&self) -> usize {
        (self.width as usize).saturating_mul(self.height as usize).saturating_mul(4)
    }

    pub fn has_depth_pixels(&self) -> bool {
        self.width > 0 && self.height > 0 && self.depth_pixels.len() >= self.byte_count()
    }

    pub fn has_normal_coverage(&self) -> bool {
        self.width > 0 && self.height > 0 && self.normal_coverage.len() >= self.byte_count()
    }

    pub fn sample_settings(&self) -> SampleSettings {
        SampleSettings {
            invert: self.invert,
            back_depth: self.back_depth,
            front_depth: self.front_depth,
            depth_scale: self.sample_depth_scale,
            alpha_threshold: self.alpha_threshold,
            custom_radius: normalize_custom_radius(self.custom_radius),
            convolution: self.convolution,
            channel: self.channel,
        }
    }

    pub fn apply_z_transform(&self, raw_depth: f32) -> f32 {
        raw_depth * self.z_scale + self.z_offset
    }
}

// ----------------------------------------------------------------------------------- coordinates

/// D `ngDepthDrawLayerDocumentBounds`: the layer rectangle in document pixels after xy offset and scale.
pub fn layer_document_bounds(layer: &Layer) -> ([f32; 2], [f32; 2]) {
    let scale_x = if layer.xy_scale[0] == 0.0 { 1.0 } else { layer.xy_scale[0] };
    let scale_y = if layer.xy_scale[1] == 0.0 { 1.0 } else { layer.xy_scale[1] };
    let x0 = layer.bounds.left as f32 + layer.xy_offset[0];
    let y0 = layer.bounds.top as f32 + layer.xy_offset[1];
    let x1 = x0 + layer.bounds.width as f32 * scale_x;
    let y1 = y0 + layer.bounds.height as f32 * scale_y;
    ([x0.min(x1), y0.min(y1)], [x0.max(x1), y0.max(y1)])
}

/// D `ngDepthDrawVertexDocumentPosition`: grid vertices through the grid's world matrix (row major,
/// translation in column 3), then shifted so the document centre is the origin.
pub fn document_points(vertices: &[[f32; 2]], world: &[[f64; 4]; 4], doc_width: i32, doc_height: i32) -> Vec<[f32; 2]> {
    let m = |r: usize, c: usize| world[r][c] as f32;
    vertices
        .iter()
        .map(|v| {
            let x = m(0, 0) * v[0] + m(0, 1) * v[1] + m(0, 3);
            let y = m(1, 0) * v[0] + m(1, 1) * v[1] + m(1, 3);
            [x + doc_width as f32 / 2.0, y + doc_height as f32 / 2.0]
        })
        .collect()
}

/// D `ngDepthDrawLayerPixelFromDocument`.
pub fn layer_pixel_from_document(layer: &Layer, document_point: [f32; 2]) -> [f32; 2] {
    let scale_x = if layer.xy_scale[0] == 0.0 { 1.0 } else { layer.xy_scale[0] };
    let scale_y = if layer.xy_scale[1] == 0.0 { 1.0 } else { layer.xy_scale[1] };
    let raw = [document_point[0] - layer.bounds.left as f32, document_point[1] - layer.bounds.top as f32];
    [(raw[0] - layer.xy_offset[0]) / scale_x, (raw[1] - layer.xy_offset[1]) / scale_y]
}

// ----------------------------------------------------------------------------------- the composer

#[derive(Clone, Debug, Default, PartialEq)]
pub struct LayerComposeStats {
    pub layer_id: String,
    pub sampled_vertices: usize,
    pub missing_vertices: usize,
    pub winning_vertices: usize,
    pub contributed_vertices: usize,
    pub additive_merge: bool,
    pub has_depth_range: bool,
    pub min_depth: f32,
    pub max_depth: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ComposeResult {
    pub target_grid_uuid: u64,
    pub depths: Vec<f32>,
    /// Empty string where no layer won the vertex.
    pub winning_layer_ids: Vec<String>,
    pub layer_stats: Vec<LayerComposeStats>,
    pub sampled_vertices: usize,
    pub missing_vertices: usize,
    pub has_depth_range: bool,
    pub min_depth: f32,
    pub max_depth: f32,
}

/// One target grid: its uuid, vertices in document space (see [`document_points`]) and base depths.
pub struct Target<'a> {
    pub grid_uuid: u64,
    pub document_points: &'a [[f32; 2]],
    pub base_depths: &'a [f32],
}

fn merge_depth(policy: MergePolicy, current: &mut f32, has_composed: &mut bool, base: f32, sampled: f32) -> bool {
    match policy {
        MergePolicy::Replace => {
            *current = sampled;
            *has_composed = true;
            true
        }
        MergePolicy::Frontmost => {
            if !*has_composed || sampled > *current {
                *current = sampled;
                *has_composed = true;
                true
            } else {
                false
            }
        }
        MergePolicy::Backmost => {
            if !*has_composed || sampled < *current {
                *current = sampled;
                *has_composed = true;
                true
            } else {
                false
            }
        }
        MergePolicy::Add => {
            *current = (if *has_composed { *current } else { base }) + sampled;
            *has_composed = true;
            true
        }
        MergePolicy::KeepExistingWhereMissing => {
            if !*has_composed {
                *current = sampled;
                *has_composed = true;
                true
            } else {
                false
            }
        }
    }
}

fn include_range(has: &mut bool, min: &mut f32, max: &mut f32, depth: f32) {
    if !depth.is_finite() {
        return;
    }
    if !*has {
        *has = true;
        *min = depth;
        *max = depth;
    } else {
        *min = min.min(depth);
        *max = max.max(depth);
    }
}

/// D `ngComposeDepthDrawTarget`: sample every bound, enabled, visible layer at each vertex and merge.
///
/// Layers are applied in `(binding.order, binding index)` order. A vertex that no layer samples keeps its
/// base depth and an empty winner. `base_depths` shorter than the vertex list is padded with zeros.
pub fn compose_target(layers: &[Layer], bindings: &[Binding], target: &Target) -> Result<ComposeResult, String> {
    let count = target.document_points.len();
    let mut base: Vec<f32> = target.base_depths.to_vec();
    base.resize(count, 0.0);
    let mut result = ComposeResult {
        target_grid_uuid: target.grid_uuid,
        depths: base.clone(),
        winning_layer_ids: vec![String::new(); count],
        ..Default::default()
    };
    let mut has_composed = vec![false; count];

    let mut entries: Vec<(&Layer, &Binding, usize)> = Vec::new();
    for (index, binding) in bindings.iter().enumerate().filter(|(_, b)| b.applies_to(target.grid_uuid)) {
        let Some(layer) = layers.iter().find(|l| l.id == binding.layer_id) else { continue };
        if !layer.enabled || !layer.visible || !layer.has_depth_pixels() {
            continue;
        }
        entries.push((layer, binding, index));
    }
    entries.sort_by_key(|(_, binding, index)| (binding.order, *index));

    for (layer, binding, _) in entries {
        let mut stats = LayerComposeStats {
            layer_id: layer.id.clone(),
            additive_merge: binding.merge_policy == MergePolicy::Add,
            ..Default::default()
        };
        let mut settings = layer.sample_settings();
        settings.alpha_threshold = settings.alpha_threshold.max(binding.coverage_threshold);
        let coverage = (binding.use_normal_layer_alpha && layer.has_normal_coverage()).then(|| Coverage {
            rgba: &layer.normal_coverage,
            width: layer.width,
            height: layer.height,
            opacity: 1.0,
        });
        for (i, point) in target.document_points.iter().enumerate() {
            let pixel = layer_pixel_from_document(layer, *point);
            let sample = sample_rgba(
                &layer.depth_pixels,
                layer.width,
                layer.height,
                coverage,
                layer.opacity,
                pixel[0],
                pixel[1],
                &settings,
            )?;
            if !sample.valid {
                stats.missing_vertices += 1;
                result.missing_vertices += 1;
                continue;
            }
            let depth = layer.apply_z_transform(sample.value);
            stats.sampled_vertices += 1;
            result.sampled_vertices += 1;
            include_range(&mut stats.has_depth_range, &mut stats.min_depth, &mut stats.max_depth, depth);
            let won = merge_depth(binding.merge_policy, &mut result.depths[i], &mut has_composed[i], base[i], depth);
            if won {
                result.winning_layer_ids[i] = layer.id.clone();
                if stats.additive_merge {
                    stats.contributed_vertices += 1;
                }
            }
        }
        result.layer_stats.push(stats);
    }

    for winner in result.winning_layer_ids.iter().filter(|w| !w.is_empty()) {
        if let Some(stats) = result.layer_stats.iter_mut().find(|s| &s.layer_id == winner) {
            stats.winning_vertices += 1;
            if !stats.additive_merge {
                stats.contributed_vertices += 1;
            }
        }
    }
    for depth in result.depths.clone() {
        include_range(&mut result.has_depth_range, &mut result.min_depth, &mut result.max_depth, depth);
    }
    Ok(result)
}

/// D `summarizeDepthDrawApply`: how many vertices an apply would change against the stored depths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApplySummary {
    pub succeeded: bool,
    pub changed_targets: usize,
    pub changed_vertices: usize,
    pub layers_applied: usize,
    pub layers_skipped: usize,
    pub missing_samples: usize,
}

pub fn count_changed_vertices(before: &[f32], after: &[f32]) -> usize {
    if before.len() != after.len() {
        return after.len();
    }
    before.iter().zip(after).filter(|(b, a)| b != a).count()
}

/// `stored_depths` is the target's current depth array, or `None` for a node that cannot hold depths
/// (D rejects deformables that are not `DepthMappedNode`: `succeeded` stays false).
pub fn summarize_apply(
    result: &ComposeResult,
    target_grid_uuid: u64,
    vertex_count: usize,
    stored_depths: Option<&[f32]>,
) -> ApplySummary {
    let mut summary = ApplySummary::default();
    if result.target_grid_uuid != target_grid_uuid || result.depths.len() != vertex_count {
        return summary;
    }
    summary.missing_samples = result.missing_vertices;
    for stats in &result.layer_stats {
        if stats.contributed_vertices > 0 {
            summary.layers_applied += 1;
        } else {
            summary.layers_skipped += 1;
        }
    }
    let Some(stored) = stored_depths else { return summary };
    summary.changed_vertices = count_changed_vertices(stored, &result.depths);
    summary.changed_targets = usize::from(summary.changed_vertices > 0);
    summary.succeeded = true;
    summary
}


// ------------------------------------------------------------------------- masks and gap cleanup

/// D `ngDepthDrawDecodeDepthPixelsFromRgba`: one byte per pixel (0 where alpha is 0).
pub fn decode_depth_pixels(rgba: &[u8], channel: Channel) -> Result<Vec<u8>, String> {
    if rgba.len() % 4 != 0 {
        return Err("RGBA buffer length must be divisible by 4".into());
    }
    Ok(rgba
        .chunks_exact(4)
        .enumerate()
        .map(|(i, px)| {
            if px[3] == 0 {
                0
            } else {
                let depth01 = pixel_depth01(rgba, i * 4, channel, false);
                ((depth01 * 255.0).round() as i64).clamp(0, 255) as u8
            }
        })
        .collect())
}

/// D `ngDepthDrawAlphaMaskFromRgba`: 1 where alpha >= `min_alpha`.
pub fn alpha_mask_from_rgba(rgba: &[u8], min_alpha: u8) -> Result<Vec<u8>, String> {
    if rgba.len() % 4 != 0 {
        return Err("RGBA buffer length must be divisible by 4".into());
    }
    Ok(rgba.chunks_exact(4).map(|px| u8::from(px[3] >= min_alpha)).collect())
}

/// D `ngDepthDrawMaskFromDepthPixels`.
pub fn mask_from_depth_pixels(depth: &[u8]) -> Vec<u8> {
    depth.iter().map(|v| u8::from(*v > 0)).collect()
}

/// D `ngDepthDrawApplyBinaryMaskToDepthPixels`: zero the depth where the mask is 0 (and past the mask).
pub fn apply_binary_mask(depth: &[u8], mask: &[u8]) -> Vec<u8> {
    depth.iter().enumerate().map(|(i, v)| if mask.get(i).is_some_and(|m| *m != 0) { *v } else { 0 }).collect()
}

const EIGHT: [[i32; 2]; 8] = [[-1, 0], [1, 0], [0, -1], [0, 1], [-1, -1], [1, -1], [-1, 1], [1, 1]];

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GapDetection {
    pub mask: Vec<u8>,
    pub zero: usize,
    pub depression: usize,
    pub cliff: usize,
    pub focused_added: usize,
    pub total: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GapFillResult {
    pub depth: Vec<u8>,
    pub filled: usize,
    pub remaining: usize,
}

const HISTOGRAM_BINS: usize = 16;
const HISTOGRAM_SHIFT: u32 = 4;
const MAX_HISTOGRAM_BYTES: u64 = 320 * 1024 * 1024;

struct HistogramIntegral {
    bins: Vec<Vec<u32>>,
    stride: usize,
}

#[derive(Default)]
struct HistogramStats {
    valid: bool,
    p50: i32,
    p65: i32,
}

pub fn histogram_dimensions_supported(width: i32, height: i32) -> bool {
    if width < 0 || height < 0 {
        return false;
    }
    let cells = (width as u64 + 1) * (height as u64 + 1);
    cells <= MAX_HISTOGRAM_BYTES / (HISTOGRAM_BINS as u64 * 4)
}

fn build_histogram(depth: &[u8], alpha: &[u8], width: i32, height: i32) -> Result<HistogramIntegral, String> {
    if !histogram_dimensions_supported(width, height) {
        return Err("Depth layer is too large for alpha-depth gap cleanup".into());
    }
    let (w, h) = (width as usize, height as usize);
    let stride = w + 1;
    let mut bins = vec![vec![0u32; stride * (h + 1)]; HISTOGRAM_BINS];
    for y in 1..=h {
        let mut row_counts = [0u32; HISTOGRAM_BINS];
        for x in 1..=w {
            let source = (y - 1) * w + x - 1;
            let value = depth[source];
            if alpha[source] != 0 && value > 0 {
                row_counts[usize::from(value >> HISTOGRAM_SHIFT)] += 1;
            }
            let out = y * stride + x;
            let prev = (y - 1) * stride + x;
            for bin in 0..HISTOGRAM_BINS {
                bins[bin][out] = bins[bin][prev] + row_counts[bin];
            }
        }
    }
    Ok(HistogramIntegral { bins, stride })
}

fn range_sum(integral: &[u32], stride: usize, x0: usize, y0: usize, x1: usize, y1: usize) -> u32 {
    integral[y1 * stride + x1]
        .wrapping_sub(integral[y0 * stride + x1])
        .wrapping_sub(integral[y1 * stride + x0])
        .wrapping_add(integral[y0 * stride + x0])
}

fn local_histogram_stats(hist: &HistogramIntegral, width: i32, height: i32, x: i32, y: i32, radius: i32, center: i32) -> HistogramStats {
    let x0 = (x - radius).max(0) as usize;
    let y0 = (y - radius).max(0) as usize;
    let x1 = (x + radius + 1).min(width) as usize;
    let y1 = (y + radius + 1).min(height) as usize;
    let count_of = |bin: usize| -> u32 {
        let count = range_sum(&hist.bins[bin], hist.stride, x0, y0, x1, y1);
        if center > 0 && bin == (center >> HISTOGRAM_SHIFT) as usize {
            count.saturating_sub(1)
        } else {
            count
        }
    };
    let total: u32 = (0..HISTOGRAM_BINS).map(count_of).sum();
    let mut stats = HistogramStats::default();
    if total < 10 {
        return stats;
    }
    let target = |fraction: f64| ((total as f64 * fraction).ceil() as u32).max(1);
    let (t50, t65, t80) = (target(0.50), target(0.65), target(0.80));
    let mut cumulative = 0u32;
    stats.valid = true;
    stats.p50 = 255;
    stats.p65 = 255;
    for bin in 0..HISTOGRAM_BINS {
        cumulative += count_of(bin);
        let value = (((bin as i32) << HISTOGRAM_SHIFT) + (1 << (HISTOGRAM_SHIFT - 1))).min(255);
        if stats.p50 == 255 && cumulative >= t50 {
            stats.p50 = value;
        }
        if stats.p65 == 255 && cumulative >= t65 {
            stats.p65 = value;
        }
        if cumulative >= t80 {
            break;
        }
    }
    stats
}

/// D `ngDepthDrawDetectAlphaDepthGaps` (depth-draw `detectAlphaDepthGaps`): holes (depth 0 under alpha),
/// local depressions and cliffs, plus the focused rules that belong to `layer_index`.
pub fn detect_alpha_depth_gaps(
    depth: &[u8],
    alpha_mask: &[u8],
    width: i32,
    height: i32,
    layer_index: i32,
    focused_rules: &[FocusedRule],
) -> Result<GapDetection, String> {
    if width < 0 || height < 0 {
        return Err("Depth dimensions must be non-negative".into());
    }
    if depth.len() != width as usize * height as usize {
        return Err("Depth length must match dimensions".into());
    }
    if alpha_mask.len() != depth.len() {
        return Err("Alpha mask length must match depth".into());
    }
    let mut result = GapDetection { mask: vec![0; depth.len()], ..Default::default() };
    let hist = build_histogram(depth, alpha_mask, width, height)?;
    for y in 0..height {
        for x in 0..width {
            let index = (y * width + x) as usize;
            if alpha_mask[index] == 0 {
                continue;
            }
            let value = i32::from(depth[index]);
            if value <= 0 {
                result.mask[index] = 1;
                result.zero += 1;
                continue;
            }
            let s16 = local_histogram_stats(&hist, width, height, x, y, 16, value);
            let s30 = local_histogram_stats(&hist, width, height, x, y, 30, value);
            let expected = (if s16.valid { s16.p65 } else { 0 }).max(if s30.valid { s30.p65 } else { 0 });
            if expected > 0 && expected - value >= 12.max((f64::from(expected) * 0.075).round() as i32) {
                result.mask[index] = 1;
                result.depression += 1;
            }
        }
    }
    for y in 1..1.max(height - 1) {
        for x in 1..1.max(width - 1) {
            let index = (y * width + x) as usize;
            if alpha_mask[index] == 0 {
                continue;
            }
            let value = i32::from(depth[index]);
            let mut high = 0i32;
            for off in EIGHT {
                let s = ((y + off[1]) * width + x + off[0]) as usize;
                if alpha_mask[s] != 0 && i32::from(depth[s]) > high {
                    high = i32::from(depth[s]);
                }
            }
            if high - value >= 16.max((f64::from(high) * 0.10).round() as i32) {
                result.mask[index] = 1;
                result.cliff += 1;
            }
        }
    }
    for rule in focused_rules.iter().filter(|r| r.layer_index == layer_index) {
        let x0 = i64::from(rule.x).max(0);
        let y0 = i64::from(rule.y).max(0);
        let x1 = (i64::from(rule.x) + i64::from(rule.w)).min(i64::from(width));
        let y1 = (i64::from(rule.y) + i64::from(rule.h)).min(i64::from(height));
        let radius = rule.radius.clamp(0, width.max(height));
        for y in y0..y1 {
            for x in x0..x1 {
                let index = (y * i64::from(width) + x) as usize;
                if alpha_mask[index] == 0 {
                    continue;
                }
                let value = i32::from(depth[index]);
                let stats = local_histogram_stats(&hist, width, height, x as i32, y as i32, radius, value);
                let expected = if stats.valid { if stats.p65 != 0 { stats.p65 } else { stats.p50 } } else { 0 };
                if value <= 0 || (expected > 0 && expected - value >= rule.lift) {
                    if result.mask[index] == 0 {
                        result.focused_added += 1;
                    }
                    result.mask[index] = 1;
                }
            }
        }
    }
    result.total = result.mask.iter().filter(|v| **v != 0).count();
    Ok(result)
}

fn median_u8(values: &mut [u8]) -> u8 {
    values.sort_unstable();
    values[values.len() / 2]
}

fn cleanup_median_u8(values: &mut [u8]) -> u8 {
    values.sort_unstable();
    values[(values.len() - 1) >> 1]
}

fn neighbor_median(depth: &[u8], alpha: &[u8], pending: &[u8], width: i32, height: i32, index: usize) -> u8 {
    let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
    let mut values = Vec::new();
    for off in EIGHT {
        let (sx, sy) = (x + off[0], y + off[1]);
        if sx < 0 || sx >= width || sy < 0 || sy >= height {
            continue;
        }
        let s = (sy * width + sx) as usize;
        if alpha[s] != 0 && pending[s] == 0 && depth[s] > 0 {
            values.push(depth[s]);
        }
    }
    if values.is_empty() {
        0
    } else {
        median_u8(&mut values)
    }
}

fn fallback_median(depth: &[u8], alpha: &[u8], pending: &[u8], width: i32, height: i32, x: i32, y: i32) -> u8 {
    for radius in [6i32, 12, 24, 48, 96] {
        let mut values = Vec::new();
        for dy in -radius..=radius {
            for dx in -radius..=radius {
                let d2 = dx * dx + dy * dy;
                if d2 <= 0 || d2 > radius * radius {
                    continue;
                }
                let (sx, sy) = (x + dx, y + dy);
                if sx < 0 || sx >= width || sy < 0 || sy >= height {
                    continue;
                }
                let s = (sy * width + sx) as usize;
                if alpha[s] != 0 && pending[s] == 0 && depth[s] > 0 {
                    values.push(depth[s]);
                }
            }
        }
        if !values.is_empty() {
            return median_u8(&mut values);
        }
    }
    0
}

/// D `ngDepthDrawMedianFillDepth` (depth-draw `medianFillDepth`): fill the masked pixels from their
/// neighbours' median, growing inward from the already valid pixels.
pub fn median_fill_depth(
    source: &[u8],
    alpha_mask: &[u8],
    fill_mask: &[u8],
    width: i32,
    height: i32,
) -> Result<GapFillResult, String> {
    if width < 0 || height < 0 {
        return Err("Depth dimensions must be non-negative".into());
    }
    if source.len() != width as usize * height as usize {
        return Err("Depth length must match dimensions".into());
    }
    if alpha_mask.len() != source.len() {
        return Err("Alpha mask length must match depth".into());
    }
    if fill_mask.len() != source.len() {
        return Err("Fill mask length must match depth".into());
    }
    let mut result = GapFillResult { depth: source.to_vec(), ..Default::default() };
    let mut pending = fill_mask.to_vec();
    let mut queued = vec![0u8; fill_mask.len()];
    let mut pending_indices = Vec::new();
    let mut frontier = Vec::new();
    for index in 0..pending.len() {
        if pending[index] == 0 || alpha_mask[index] == 0 {
            continue;
        }
        pending_indices.push(index);
        if neighbor_median(&result.depth, alpha_mask, &pending, width, height, index) > 0 {
            queued[index] = 1;
            frontier.push(index);
        }
    }
    while !frontier.is_empty() {
        let mut updates = Vec::new();
        for &index in &frontier {
            queued[index] = 0;
            if pending[index] == 0 || alpha_mask[index] == 0 {
                continue;
            }
            let value = neighbor_median(&result.depth, alpha_mask, &pending, width, height, index);
            if value > 0 {
                updates.push((index, value));
            }
        }
        if updates.is_empty() {
            break;
        }
        let mut next = Vec::new();
        for (index, value) in updates {
            result.depth[index] = value;
            pending[index] = 0;
            result.filled += 1;
            let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
            for off in EIGHT {
                let (sx, sy) = (x + off[0], y + off[1]);
                if sx < 0 || sx >= width || sy < 0 || sy >= height {
                    continue;
                }
                let s = (sy * width + sx) as usize;
                if pending[s] != 0 && alpha_mask[s] != 0 && queued[s] == 0 {
                    queued[s] = 1;
                    next.push(s);
                }
            }
        }
        frontier = next;
    }
    for &index in &pending_indices {
        if pending[index] == 0 || alpha_mask[index] == 0 {
            continue;
        }
        let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
        let value = fallback_median(&result.depth, alpha_mask, &pending, width, height, x, y);
        if value > 0 {
            result.depth[index] = value;
            pending[index] = 0;
            result.filled += 1;
        }
    }
    result.remaining = pending_indices.iter().filter(|i| pending[**i] != 0).count();
    Ok(result)
}

fn is_mask_contour_pixel(mask: &[u8], width: i32, height: i32, index: usize) -> bool {
    if mask[index] == 0 {
        return false;
    }
    let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
    for off in [[-1, 0], [1, 0], [0, -1], [0, 1]] {
        let (sx, sy) = (x + off[0], y + off[1]);
        if sx < 0 || sx >= width || sy < 0 || sy >= height || mask[(sy * width + sx) as usize] == 0 {
            return true;
        }
    }
    false
}

fn check_plane(len: usize, width: i32, height: i32, what: &str) -> Result<(), String> {
    if width < 0 || height < 0 {
        return Err(format!("{what} dimensions must be non-negative"));
    }
    if len != width as usize * height as usize {
        return Err(format!("{what} length must match dimensions"));
    }
    Ok(())
}

/// D `ngDepthDrawBuildLayerContourBandMask`: the mask border, grown `thickness - 1` times inside the mask.
pub fn build_layer_contour_band_mask(mask: &[u8], width: i32, height: i32, thickness: i32) -> Result<Vec<u8>, String> {
    check_plane(mask.len(), width, height, "Mask")?;
    let mut contour = vec![0u8; mask.len()];
    let mut band = vec![0u8; mask.len()];
    for index in 0..mask.len() {
        if is_mask_contour_pixel(mask, width, height, index) {
            contour[index] = 1;
            band[index] = 1;
        }
    }
    let mut frontier = contour;
    for _ in 1..thickness.max(1) {
        let mut next = vec![0u8; mask.len()];
        for index in 0..frontier.len() {
            if frontier[index] == 0 {
                continue;
            }
            let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
            for off in EIGHT {
                let (sx, sy) = (x + off[0], y + off[1]);
                if sx < 0 || sx >= width || sy < 0 || sy >= height {
                    continue;
                }
                let s = (sy * width + sx) as usize;
                if mask[s] == 0 || band[s] != 0 {
                    continue;
                }
                band[s] = 1;
                next[s] = 1;
            }
        }
        frontier = next;
    }
    Ok(band)
}

/// D `ngDepthDrawErodePositiveDepthMask`: positive-depth pixels, eroded `thickness` times (8-neighbourhood).
pub fn erode_positive_depth_mask(depth: &[u8], width: i32, height: i32, thickness: i32) -> Result<Vec<u8>, String> {
    check_plane(depth.len(), width, height, "Depth")?;
    let mut mask = mask_from_depth_pixels(depth);
    for _ in 0..thickness.max(0) {
        let mut eroded = vec![0u8; mask.len()];
        for index in 0..mask.len() {
            if mask[index] == 0 {
                continue;
            }
            let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
            let keep = EIGHT.iter().all(|off| {
                let (sx, sy) = (x + off[0], y + off[1]);
                sx >= 0 && sx < width && sy >= 0 && sy < height && mask[(sy * width + sx) as usize] != 0
            });
            if keep {
                eroded[index] = 1;
            }
        }
        mask = eroded;
    }
    Ok(mask)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct InpaintResult {
    pub pixels: Vec<u8>,
    pub filled_mask: Vec<u8>,
}

fn has_positive_masked_neighbor(depth: &[u8], mask: &[u8], width: i32, height: i32, index: usize) -> bool {
    let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
    EIGHT.iter().any(|off| {
        let (sx, sy) = (x + off[0], y + off[1]);
        if sx < 0 || sx >= width || sy < 0 || sy >= height {
            return false;
        }
        let s = (sy * width + sx) as usize;
        mask[s] != 0 && depth[s] > 0
    })
}

fn masked_neighbor_median(depth: &[u8], mask: &[u8], width: i32, height: i32, index: usize) -> u8 {
    let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
    let mut values = Vec::new();
    for off in EIGHT {
        let (sx, sy) = (x + off[0], y + off[1]);
        if sx < 0 || sx >= width || sy < 0 || sy >= height {
            continue;
        }
        let s = (sy * width + sx) as usize;
        if mask[s] != 0 && depth[s] > 0 {
            values.push(depth[s]);
        }
    }
    if values.is_empty() {
        0
    } else {
        cleanup_median_u8(&mut values)
    }
}

fn nearest_masked_depth(depth: &[u8], mask: &[u8], width: i32, height: i32, tx: i32, ty: i32, search: i32) -> u8 {
    let mut best = 0u8;
    let mut best_d2 = i32::MAX;
    for dy in -search..=search {
        let y = ty + dy;
        if y < 0 || y >= height {
            continue;
        }
        for dx in -search..=search {
            let x = tx + dx;
            if x < 0 || x >= width {
                continue;
            }
            let index = (y * width + x) as usize;
            if mask[index] == 0 || depth[index] == 0 {
                continue;
            }
            let d2 = dx * dx + dy * dy;
            if d2 < best_d2 {
                best_d2 = d2;
                best = depth[index];
            }
        }
    }
    best
}

fn percentile(sorted: &[u8], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let index = (((sorted.len() - 1) as f64 * p).round() as i64).clamp(0, sorted.len() as i64 - 1) as usize;
    f64::from(sorted[index])
}

struct Estimate {
    depth: f64,
    confidence: f64,
    weight: f64,
}

fn estimate_at_scale(depth: &[u8], mask: &[u8], width: i32, height: i32, index: usize, radius: i32, min_samples: usize, weight: f64) -> Option<Estimate> {
    let (x0, y0) = ((index % width as usize) as i32, (index / width as usize) as i32);
    let search = 1.max((f64::from(radius) / 3.0).floor() as i32);
    let mut grid = [0.0f64; 9];
    let mut values: Vec<u8> = Vec::new();
    let mut cursor = 0;
    for gy in -1..2 {
        for gx in -1..2 {
            let tx = (x0 + gx * radius).clamp(0, width - 1);
            let ty = (y0 + gy * radius).clamp(0, height - 1);
            let sampled = nearest_masked_depth(depth, mask, width, height, tx, ty, search);
            grid[cursor] = f64::from(sampled);
            cursor += 1;
            if sampled > 0 {
                values.push(sampled);
            }
        }
    }
    if values.len() < min_samples {
        return None;
    }
    let mut sorted = values.clone();
    sorted.sort_unstable();
    let center_mean = values.iter().map(|v| f64::from(*v)).sum::<f64>() / values.len() as f64;
    for v in grid.iter_mut() {
        if *v <= 0.0 {
            *v = center_mean;
        }
    }
    let grad_x = (grid[2] + 2.0 * grid[5] + grid[8]) - (grid[0] + 2.0 * grid[3] + grid[6]);
    let grad_y = (grid[6] + 2.0 * grid[7] + grid[8]) - (grid[0] + 2.0 * grid[1] + grid[2]);
    let plane = grid[4] + (grad_x + grad_y) * 0.125;
    let median = f64::from(cleanup_median_u8(&mut values.clone()));
    let lo = percentile(&sorted, 0.2);
    let hi = percentile(&sorted, 0.8);
    let robust = lo.max(hi.min((plane * 0.7 + median * 0.3).round()));
    Some(Estimate { depth: robust, confidence: (values.len() as f64 / 9.0).clamp(0.0, 1.0), weight })
}

fn multiscale_depth(depth: &[u8], mask: &[u8], width: i32, height: i32, index: usize) -> u8 {
    let (mut weighted, mut total) = (0.0f64, 0.0f64);
    for (radius, min_samples, weight) in [(1, 4, 0.48), (7, 5, 0.32), (37, 5, 0.20)] {
        if let Some(e) = estimate_at_scale(depth, mask, width, height, index, radius, min_samples, weight) {
            let confidence = e.weight * e.confidence;
            weighted += e.depth * confidence;
            total += confidence;
        }
    }
    if total > 0.0 {
        return (weighted / total).round().clamp(0.0, 255.0) as u8;
    }
    masked_neighbor_median(depth, mask, width, height, index)
}

/// D `ngDepthDrawInpaintMaskedLayerDepth` (depth-draw `inpaintMaskedLayerDepth`): fill zero-depth pixels
/// inside the mask from a three-scale plane estimate, starting next to valid pixels.
pub fn inpaint_masked_layer_depth(source: &[u8], mask: &[u8], width: i32, height: i32) -> Result<InpaintResult, String> {
    check_plane(source.len(), width, height, "Depth")?;
    if mask.len() != source.len() {
        return Err("Mask length must match depth".into());
    }
    let mut result = InpaintResult { pixels: source.to_vec(), filled_mask: vec![0; source.len()] };
    let mut queue: Vec<usize> = Vec::new();
    let mut queued = vec![0u8; source.len()];
    for index in 0..source.len() {
        if mask[index] == 0 || result.pixels[index] > 0 {
            continue;
        }
        if has_positive_masked_neighbor(&result.pixels, mask, width, height, index) {
            queue.push(index);
            queued[index] = 1;
        }
    }
    let mut head = 0;
    while head < queue.len() {
        let index = queue[head];
        head += 1;
        queued[index] = 0;
        if mask[index] == 0 || result.pixels[index] > 0 {
            continue;
        }
        let fill = multiscale_depth(&result.pixels, mask, width, height, index);
        if fill == 0 {
            continue;
        }
        result.pixels[index] = fill;
        result.filled_mask[index] = 1;
        let (x, y) = ((index % width as usize) as i32, (index / width as usize) as i32);
        for off in EIGHT {
            let (sx, sy) = (x + off[0], y + off[1]);
            if sx < 0 || sx >= width || sy < 0 || sy >= height {
                continue;
            }
            let s = (sy * width + sx) as usize;
            if mask[s] == 0 || result.pixels[s] > 0 || queued[s] != 0 {
                continue;
            }
            queue.push(s);
            queued[s] = 1;
        }
    }
    Ok(result)
}

// -------------------------------------------------------------------------- layer cleanup (session)

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct GapFillSummary {
    pub succeeded: bool,
    pub changed: bool,
    pub layer_id: String,
    pub detected: GapDetection,
    pub filled: GapFillResult,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContourRepairSummary {
    pub succeeded: bool,
    pub changed: bool,
    pub layer_id: String,
    pub contour_pixels: usize,
    pub filled_pixels: usize,
}

fn write_cleanup_depth_pixel(layer: &mut Layer, index: usize, value: u8) -> bool {
    let offset = index * 4;
    let px = &mut layer.depth_pixels[offset..offset + 3];
    let channels: &[usize] = match layer.channel {
        Channel::R => &[0],
        Channel::G => &[1],
        Channel::B => &[2],
        Channel::AverageRgb | Channel::Luminance => &[0, 1, 2],
    };
    let mut changed = false;
    for &c in channels {
        changed |= px[c] != value;
        px[c] = value;
    }
    changed
}

fn layer_masks(layer: &Layer) -> Result<(Vec<u8>, Vec<u8>), String> {
    let depth = decode_depth_pixels(&layer.depth_pixels, layer.channel)?;
    let mask = if layer.alpha_mask.len() == depth.len() {
        layer.alpha_mask.clone()
    } else {
        alpha_mask_from_rgba(&layer.depth_pixels, 1)?
    };
    Ok((depth, mask))
}

/// D `DepthDrawSession.applyLayerAlphaDepthGapFill`. `layer_index` is the layer's position in the session
/// (the focused rules name it). Records a replayable cleanup operation when the pixels changed.
pub fn apply_layer_alpha_depth_gap_fill(layer: &mut Layer, layer_index: i32, rules: &[FocusedRule]) -> Result<GapFillSummary, String> {
    let mut summary = GapFillSummary { layer_id: layer.id.clone(), ..Default::default() };
    if !layer.has_depth_pixels() {
        return Ok(summary);
    }
    let rules: Vec<FocusedRule> = rules.iter().map(|r| normalize_focused_rule(*r, layer.width, layer.height)).collect();
    let (depth, alpha) = layer_masks(layer)?;
    summary.detected = detect_alpha_depth_gaps(&depth, &alpha, layer.width, layer.height, layer_index, &rules)?;
    summary.filled = median_fill_depth(&depth, &alpha, &summary.detected.mask, layer.width, layer.height)?;
    if summary.detected.total == 0 && summary.filled.filled == 0 && summary.filled.remaining == 0 {
        summary.succeeded = true;
        return Ok(summary);
    }
    let mut changed = false;
    for (i, value) in summary.filled.depth.iter().enumerate() {
        if summary.detected.mask[i] == 0 || depth[i] == *value {
            continue;
        }
        changed |= write_cleanup_depth_pixel(layer, i, *value);
    }
    layer.alpha_mask = alpha;
    if changed {
        layer.cleanup_operations.push(CleanupOperation {
            kind: CleanupKind::AlphaDepthGapFill,
            contour_thickness: 2,
            focused_rules: rules,
        });
    }
    summary.changed = changed;
    summary.succeeded = true;
    Ok(summary)
}

/// D `DepthDrawSession.repairLayerContourDepth`.
pub fn repair_layer_contour_depth(layer: &mut Layer, thickness: i32) -> Result<ContourRepairSummary, String> {
    let mut summary = ContourRepairSummary { layer_id: layer.id.clone(), ..Default::default() };
    if !layer.has_depth_pixels() {
        return Ok(summary);
    }
    let thickness = normalize_contour_thickness(thickness);
    let (depth, mask) = layer_masks(layer)?;
    let band = build_layer_contour_band_mask(&mask, layer.width, layer.height, thickness)?;
    let mut seed = depth;
    for (i, value) in band.iter().enumerate() {
        if *value != 0 {
            seed[i] = 0;
            summary.contour_pixels += 1;
        }
    }
    let repaired = inpaint_masked_layer_depth(&seed, &mask, layer.width, layer.height)?;
    summary.filled_pixels = repaired.filled_mask.iter().filter(|v| **v != 0).count();
    let mut changed = false;
    for (i, value) in repaired.pixels.iter().enumerate() {
        if repaired.filled_mask[i] != 0 {
            changed |= write_cleanup_depth_pixel(layer, i, *value);
        }
    }
    layer.alpha_mask = mask;
    if changed {
        layer.cleanup_operations.push(CleanupOperation {
            kind: CleanupKind::ContourRepair,
            contour_thickness: thickness,
            focused_rules: Vec::new(),
        });
    }
    summary.changed = changed;
    summary.succeeded = true;
    Ok(summary)
}

/// D `replayLayerCleanupOperations`: redo the recorded cleanup on freshly loaded source pixels.
/// Returns false for a layer without depth pixels.
pub fn replay_layer_cleanup_operations(layer: &mut Layer, layer_index: i32) -> Result<bool, String> {
    if !layer.has_depth_pixels() {
        return Ok(false);
    }
    let mut operations = std::mem::take(&mut layer.cleanup_operations);
    for operation in &mut operations {
        match operation.kind {
            CleanupKind::AlphaDepthGapFill => {
                let rules: Vec<FocusedRule> =
                    operation.focused_rules.iter().map(|r| FocusedRule { layer_index, ..*r }).collect();
                apply_layer_alpha_depth_gap_fill(layer, layer_index, &rules)?;
            }
            CleanupKind::ContourRepair => {
                operation.contour_thickness = normalize_contour_thickness(operation.contour_thickness);
                repair_layer_contour_depth(layer, operation.contour_thickness)?;
            }
        }
    }
    layer.cleanup_operations = operations;
    Ok(true)
}


// ------------------------------------------------------------------------------------ the session

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DisplayOptions {
    pub show_normal_image: bool,
    pub show_raw_depth: bool,
    pub show_coverage: bool,
    pub show_composite: bool,
    pub show_layer_planes: bool,
    pub show_depth_ranges: bool,
    pub show_missing_vertices: bool,
    pub show_winning_layer: bool,
    pub use_gpu_preview: bool,
}

impl Default for DisplayOptions {
    fn default() -> Self {
        Self {
            show_normal_image: true,
            show_raw_depth: false,
            show_coverage: false,
            show_composite: true,
            show_layer_planes: true,
            show_depth_ranges: true,
            show_missing_vertices: true,
            show_winning_layer: false,
            use_gpu_preview: false,
        }
    }
}

/// D `DepthDrawSession`: layers, bindings, selection and the preview-dirty bookkeeping.
#[derive(Clone, Debug, Default)]
pub struct Session {
    pub layers: Vec<Layer>,
    pub bindings: Vec<Binding>,
    pub selected_layer_id: String,
    pub selected_grid_uuid: u64,
    pub display: DisplayOptions,
    pub source_identity: String,
    pub document_width: i32,
    pub document_height: i32,
    preview_dirty: BTreeSet<u64>,
    preview_revisions: BTreeMap<u64, u64>,
    diagnostics_revision: u64,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    fn bump_diagnostics(&mut self) {
        self.diagnostics_revision = self.diagnostics_revision.wrapping_add(1);
        if self.diagnostics_revision == 0 {
            self.diagnostics_revision = 1;
        }
    }

    pub fn diagnostics_revision(&self) -> u64 {
        self.diagnostics_revision
    }

    pub fn layer_index(&self, id: &str) -> Option<usize> {
        self.layers.iter().position(|l| l.id == id)
    }

    pub fn layer_by_id(&self, id: &str) -> Option<&Layer> {
        self.layers.iter().find(|l| l.id == id)
    }

    pub fn bindings_for_grid(&self, grid_uuid: u64) -> Vec<&Binding> {
        self.bindings.iter().filter(|b| b.applies_to(grid_uuid)).collect()
    }

    pub fn has_target_grid(&self, grid_uuid: u64) -> bool {
        grid_uuid != 0 && self.bindings.iter().any(|b| b.target_grid_uuid == grid_uuid)
    }

    pub fn mark_target_preview_dirty(&mut self, grid_uuid: u64) {
        if grid_uuid == 0 {
            return;
        }
        self.bump_diagnostics();
        let revision = self.preview_revisions.entry(grid_uuid).or_insert(0);
        *revision = revision.wrapping_add(1).max(1);
        self.preview_dirty.insert(grid_uuid);
    }

    pub fn target_preview_revision(&self, grid_uuid: u64) -> u64 {
        self.preview_revisions.get(&grid_uuid).copied().unwrap_or(0)
    }

    pub fn mark_layer_preview_dirty(&mut self, layer_id: &str) {
        if layer_id.is_empty() {
            return;
        }
        self.bump_diagnostics();
        let targets: Vec<u64> =
            self.bindings.iter().filter(|b| b.enabled && b.layer_id == layer_id).map(|b| b.target_grid_uuid).collect();
        for target in targets {
            self.mark_target_preview_dirty(target);
        }
    }

    pub fn mark_all_preview_dirty(&mut self) {
        self.bump_diagnostics();
        let targets: Vec<u64> = self.bindings.iter().filter(|b| b.enabled).map(|b| b.target_grid_uuid).collect();
        for target in targets {
            self.mark_target_preview_dirty(target);
        }
    }

    pub fn is_target_preview_dirty(&self, grid_uuid: u64) -> bool {
        self.preview_dirty.contains(&grid_uuid)
    }

    /// Dirty target uuids in ascending order.
    pub fn dirty_target_grid_ids(&self) -> Vec<u64> {
        self.preview_dirty.iter().copied().collect()
    }

    pub fn clear_target_preview_dirty(&mut self, grid_uuid: u64) {
        self.preview_dirty.remove(&grid_uuid);
    }

    pub fn clear_preview_dirty(&mut self) {
        self.preview_dirty.clear();
    }

    pub fn select_layer(&mut self, id: &str) -> bool {
        if self.layer_by_id(id).is_none() {
            return false;
        }
        self.selected_layer_id = id.to_owned();
        true
    }

    pub fn select_target_grid(&mut self, grid_uuid: u64) -> bool {
        if !self.has_target_grid(grid_uuid) {
            return false;
        }
        self.selected_grid_uuid = grid_uuid;
        true
    }

    /// D `selectLayerPlaneAtDocumentPoint`: the topmost enabled, visible layer whose rectangle holds the
    /// point becomes the selection, together with the first enabled binding's target.
    pub fn select_layer_plane_at_document_point(&mut self, point: [f32; 2]) -> bool {
        let hit = self.layers.iter().rev().find(|layer| {
            if !layer.enabled || !layer.visible {
                return false;
            }
            let (min, max) = layer_document_bounds(layer);
            !(point[0] < min[0] || point[0] > max[0] || point[1] < min[1] || point[1] > max[1])
        });
        let Some(layer) = hit else { return false };
        let id = layer.id.clone();
        if let Some(binding) = self.bindings.iter().find(|b| b.enabled && b.layer_id == id) {
            self.selected_grid_uuid = binding.target_grid_uuid;
        }
        self.selected_layer_id = id;
        true
    }

    pub fn update_layer_xy_transform(&mut self, id: &str, offset: [f32; 2], scale: [f32; 2]) -> bool {
        let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) else { return false };
        layer.xy_offset = offset;
        layer.xy_scale = scale;
        self.mark_layer_preview_dirty(id);
        true
    }

    pub fn update_layer_z_transform(&mut self, id: &str, back: f32, front: f32, invert: bool, z_scale: f32, z_offset: f32) -> bool {
        let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) else { return false };
        layer.back_depth = back;
        layer.front_depth = front;
        layer.invert = invert;
        layer.z_scale = z_scale;
        layer.z_offset = z_offset;
        self.mark_layer_preview_dirty(id);
        true
    }

    pub fn update_layer_sampling(&mut self, id: &str, channel: Channel, convolution: Convolution, custom_radius: i32, alpha_threshold: f32) -> bool {
        let gpu = self.display.use_gpu_preview;
        let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) else { return false };
        // The GPU preview has no custom-radius median.
        layer.convolution = if gpu && convolution == Convolution::MedianCustom { Convolution::Median3x3 } else { convolution };
        layer.channel = channel;
        layer.custom_radius = normalize_custom_radius(custom_radius);
        layer.alpha_threshold = alpha_threshold;
        self.mark_layer_preview_dirty(id);
        true
    }

    pub fn update_layer_visibility(&mut self, id: &str, visible: bool, enabled: bool) -> bool {
        let Some(layer) = self.layers.iter_mut().find(|l| l.id == id) else { return false };
        layer.visible = visible;
        layer.enabled = enabled;
        self.mark_layer_preview_dirty(id);
        true
    }

    pub fn update_binding_sampling(&mut self, layer_id: &str, grid_uuid: u64, use_normal_layer_alpha: bool, coverage_threshold: f32) -> bool {
        let Some(binding) = self.bindings.iter_mut().find(|b| b.layer_id == layer_id && b.target_grid_uuid == grid_uuid) else {
            return false;
        };
        binding.use_normal_layer_alpha = use_normal_layer_alpha;
        binding.coverage_threshold = coverage_threshold;
        let (enabled, target) = (binding.enabled, binding.target_grid_uuid);
        self.bump_diagnostics();
        if enabled {
            self.mark_target_preview_dirty(target);
        }
        true
    }

    pub fn update_binding_state(&mut self, layer_id: &str, grid_uuid: u64, enabled: bool, order: i32, policy: MergePolicy) -> bool {
        let Some(binding) = self.bindings.iter_mut().find(|b| b.layer_id == layer_id && b.target_grid_uuid == grid_uuid) else {
            return false;
        };
        binding.enabled = enabled;
        binding.order = order;
        binding.merge_policy = policy;
        self.mark_target_preview_dirty(grid_uuid);
        true
    }

    /// With the GPU preview on, custom-radius median falls back to 3x3 (the shader cannot run it).
    pub fn normalize_gpu_preview_sampling(&mut self) -> bool {
        if !self.display.use_gpu_preview {
            return false;
        }
        let ids: Vec<String> = self
            .layers
            .iter_mut()
            .filter(|l| l.convolution == Convolution::MedianCustom)
            .map(|l| {
                l.convolution = Convolution::Median3x3;
                l.id.clone()
            })
            .collect();
        for id in &ids {
            self.mark_layer_preview_dirty(id);
        }
        !ids.is_empty()
    }

    pub fn update_display_options(&mut self, next: DisplayOptions) -> bool {
        let display_changed = self.display != next;
        self.display = next;
        let sampling_changed = self.normalize_gpu_preview_sampling();
        if !display_changed && !sampling_changed {
            return false;
        }
        self.mark_all_preview_dirty();
        true
    }

    pub fn apply_layer_alpha_depth_gap_fill(&mut self, id: &str, rules: &[FocusedRule]) -> Result<GapFillSummary, String> {
        let Some(index) = self.layer_index(id) else {
            return Ok(GapFillSummary { layer_id: id.to_owned(), ..Default::default() });
        };
        let summary = apply_layer_alpha_depth_gap_fill(&mut self.layers[index], index as i32, rules)?;
        if summary.changed {
            self.mark_layer_preview_dirty(id);
        }
        Ok(summary)
    }

    pub fn repair_layer_contour_depth(&mut self, id: &str, thickness: i32) -> Result<ContourRepairSummary, String> {
        let Some(index) = self.layer_index(id) else {
            return Ok(ContourRepairSummary { layer_id: id.to_owned(), ..Default::default() });
        };
        let summary = repair_layer_contour_depth(&mut self.layers[index], thickness)?;
        if summary.changed {
            self.mark_layer_preview_dirty(id);
        }
        Ok(summary)
    }

    pub fn replay_layer_cleanup_operations(&mut self, id: &str) -> Result<bool, String> {
        let Some(index) = self.layer_index(id) else { return Ok(false) };
        let replayed = replay_layer_cleanup_operations(&mut self.layers[index], index as i32)?;
        if replayed && !self.layers[index].cleanup_operations.is_empty() {
            self.mark_layer_preview_dirty(id);
        }
        Ok(replayed)
    }

    pub fn clear_selection(&mut self) {
        self.selected_layer_id.clear();
        self.selected_grid_uuid = 0;
    }

    pub fn clear(&mut self) {
        self.layers.clear();
        self.bindings.clear();
        self.source_identity.clear();
        self.document_width = 0;
        self.document_height = 0;
        self.clear_selection();
        self.clear_preview_dirty();
        self.preview_revisions.clear();
        self.bump_diagnostics();
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReloadStateResult {
    pub matched_layers: usize,
    pub preserved_bindings: usize,
    pub preserved_selected_layer: bool,
    pub preserved_selected_target: bool,
}

fn reload_layer_key(layer: &Layer) -> String {
    format!("{}\0{}\0{}\0{}", layer.layer_path, layer.display_name, layer.width, layer.height)
}

fn is_flat_source(layer: &Layer) -> bool {
    let lower = layer.source_path.to_lowercase();
    [".png", ".jpg", ".jpeg", ".bmp", ".tga"].iter().any(|ext| lower.ends_with(ext))
}

/// D `ngDepthDrawCarryReloadState`: move the user's settings from `previous` onto a freshly loaded
/// session, matching layers by (path, name, size) one-to-one, and replay recorded cleanup.
pub fn carry_reload_state(reloaded: &mut Session, previous: &Session) -> Result<ReloadStateResult, String> {
    let mut result = ReloadStateResult::default();
    reloaded.display = previous.display;
    let mut id_map: BTreeMap<String, String> = BTreeMap::new();
    let mut used: BTreeSet<String> = BTreeSet::new();
    for prev in &previous.layers {
        let key = reload_layer_key(prev);
        let found = reloaded
            .layers
            .iter()
            .position(|l| !used.contains(&l.id) && reload_layer_key(l) == key)
            .or_else(|| {
                (reloaded.layers.len() == 1
                    && is_flat_source(prev)
                    && is_flat_source(&reloaded.layers[0])
                    && !used.contains(&reloaded.layers[0].id))
                .then_some(0)
            });
        let Some(index) = found else { continue };
        let layer = &mut reloaded.layers[index];
        let reloaded_id = layer.id.clone();
        used.insert(reloaded_id.clone());
        layer.visible = prev.visible;
        layer.enabled = prev.enabled;
        layer.xy_offset = prev.xy_offset;
        layer.xy_scale = prev.xy_scale;
        layer.z_offset = prev.z_offset;
        layer.z_scale = prev.z_scale;
        layer.back_depth = prev.back_depth;
        layer.front_depth = prev.front_depth;
        layer.invert = prev.invert;
        layer.channel = prev.channel;
        layer.sample_depth_scale = prev.sample_depth_scale;
        layer.convolution = prev.convolution;
        layer.custom_radius = prev.custom_radius;
        layer.alpha_threshold = prev.alpha_threshold;
        layer.cleanup_operations = prev.cleanup_operations.clone();
        replay_layer_cleanup_operations(layer, index as i32)?;
        id_map.insert(prev.id.clone(), reloaded_id);
        result.matched_layers += 1;
    }
    reloaded.bindings.clear();
    for binding in &previous.bindings {
        let Some(mapped) = id_map.get(&binding.layer_id) else { continue };
        let mut preserved = binding.clone();
        preserved.layer_id = mapped.clone();
        reloaded.bindings.push(preserved);
        result.preserved_bindings += 1;
    }
    reloaded.selected_layer_id.clear();
    if let Some(mapped) = id_map.get(&previous.selected_layer_id) {
        if reloaded.layer_by_id(mapped).is_some() {
            reloaded.selected_layer_id = mapped.clone();
            result.preserved_selected_layer = true;
        }
    }
    reloaded.selected_grid_uuid = 0;
    if previous.selected_grid_uuid != 0 && reloaded.has_target_grid(previous.selected_grid_uuid) {
        reloaded.selected_grid_uuid = previous.selected_grid_uuid;
        result.preserved_selected_target = true;
    }
    reloaded.normalize_gpu_preview_sampling();
    reloaded.mark_all_preview_dirty();
    Ok(result)
}

// ------------------------------------------------------------------------------------ auto bind

/// A scene node the auto-binder may match a layer to. The caller derives these from the model:
/// every Part, plus every depth-mapped GridDeformer / PathDeformer (`direct_target`).
#[derive(Clone, Debug, PartialEq)]
pub struct BindNode {
    pub uuid: u64,
    pub name: String,
    /// The Part's PSD layer path; empty falls back to `/name`.
    pub layer_path: String,
    pub is_part: bool,
    /// A depth-mapped grid or path (matched by name when `match_direct_grid_name`).
    pub direct_target: bool,
    /// The nearest depth-mapped ancestor-or-self: its uuid and name.
    pub target: Option<(u64, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AutoBindResult {
    pub layer_id: String,
    pub layer_path: String,
    pub layer_name: String,
    pub matched_node_name: String,
    pub target_grid_name: String,
    pub matched_node_uuid: u64,
    pub target_grid_uuid: u64,
    pub matched: bool,
    pub ambiguous: bool,
    pub status: String,
}

fn layer_base_name(path: &str) -> &str {
    path.rfind(['/', '\\']).map_or(path, |i| &path[i + 1..])
}

/// D `ngDepthDrawAutoBindLayer`. Priority: exact layer path, then path base name, then node name, then
/// (optionally) a depth-mapped grid/path of that name. Only the best priority group counts.
pub fn auto_bind_layer(nodes: &[BindNode], layer: &Layer, match_direct_grid_name: bool) -> AutoBindResult {
    let mut result = AutoBindResult {
        layer_id: layer.id.clone(),
        layer_path: layer.layer_path.clone(),
        layer_name: if layer.display_name.is_empty() {
            layer_base_name(&layer.layer_path).to_owned()
        } else {
            layer.display_name.clone()
        },
        ..Default::default()
    };
    let mut candidates: Vec<(&BindNode, u8)> = Vec::new();
    for node in nodes.iter().filter(|n| n.is_part) {
        let path = if node.layer_path.is_empty() { format!("/{}", node.name) } else { node.layer_path.clone() };
        if path == result.layer_path {
            candidates.push((node, 0));
        } else if layer_base_name(&path) == result.layer_name {
            candidates.push((node, 1));
        } else if node.name == result.layer_name {
            candidates.push((node, 2));
        }
    }
    if match_direct_grid_name {
        for node in nodes.iter().filter(|n| n.direct_target && n.name == result.layer_name) {
            candidates.push((node, 3));
        }
    }
    let Some(best) = candidates.iter().map(|(_, p)| *p).min() else {
        result.status = "Unmatched".into();
        return result;
    };
    let best: Vec<&BindNode> = candidates.iter().filter(|(_, p)| *p == best).map(|(n, _)| *n).collect();
    if best.len() > 1 {
        result.ambiguous = true;
        result.status = "Ambiguous".into();
        return result;
    }
    let node = best[0];
    result.matched_node_name = node.name.clone();
    result.matched_node_uuid = node.uuid;
    let Some((uuid, name)) = &node.target else {
        result.status = "UnmatchedWithoutGrid".into();
        return result;
    };
    result.matched = true;
    result.target_grid_name = name.clone();
    result.target_grid_uuid = *uuid;
    result.status = "Matched".into();
    result
}

/// D `ngDepthDrawBindingFromAutoBind`.
pub fn binding_from_auto_bind(result: &AutoBindResult, order: i32) -> Binding {
    Binding {
        layer_id: result.layer_id.clone(),
        target_node_uuid: result.matched_node_uuid,
        target_grid_uuid: result.target_grid_uuid,
        order,
        enabled: result.matched,
        ..Default::default()
    }
}

/// D `ngDepthDrawAutoBindSession`. `is_depth_target` answers whether a uuid still names a depth-mapped node
/// (a stale binding to a removed or non-depth node may be replaced).
pub fn auto_bind_session(
    session: &mut Session,
    nodes: &[BindNode],
    match_direct_grid_name: bool,
    is_depth_target: &dyn Fn(u64) -> bool,
) -> Vec<AutoBindResult> {
    let mut results = Vec::new();
    for i in 0..session.layers.len() {
        let layer = session.layers[i].clone();
        let result = auto_bind_layer(nodes, &layer, match_direct_grid_name);
        results.push(result.clone());
        if !result.matched {
            continue;
        }
        let mut found = false;
        let mut reusable: Option<usize> = None;
        for (index, binding) in session.bindings.iter_mut().enumerate() {
            if binding.layer_id != layer.id {
                continue;
            }
            if binding.target_grid_uuid == result.target_grid_uuid {
                binding.target_node_uuid = result.matched_node_uuid;
                binding.enabled = true;
                found = true;
                break;
            }
            if reusable.is_none() && (!binding.enabled || !is_depth_target(binding.target_grid_uuid)) {
                reusable = Some(index);
            }
        }
        if found {
            continue;
        }
        let binding = binding_from_auto_bind(&result, i as i32);
        match reusable {
            Some(index) => session.bindings[index] = binding,
            None => session.bindings.push(binding),
        }
    }
    results
}

// ------------------------------------------------------------------------------------ diagnostics

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SourceDiagnostics {
    pub total_layers: usize,
    pub usable_depth_layers: usize,
    pub disabled_layers: usize,
    pub invisible_layers: usize,
    pub layers_missing_depth_pixels: usize,
    pub layers_missing_target: usize,
    pub layers_missing_coverage: usize,
    pub layers_with_ambiguous_bindings: usize,
    pub total_bindings: usize,
    pub enabled_bindings: usize,
}

pub fn source_diagnostics(session: &Session) -> SourceDiagnostics {
    let mut r = SourceDiagnostics {
        total_layers: session.layers.len(),
        total_bindings: session.bindings.len(),
        enabled_bindings: session.bindings.iter().filter(|b| b.enabled).count(),
        ..Default::default()
    };
    for layer in &session.layers {
        r.disabled_layers += usize::from(!layer.enabled);
        r.invisible_layers += usize::from(!layer.visible);
        if !layer.has_depth_pixels() {
            r.layers_missing_depth_pixels += 1;
            continue;
        }
        r.usable_depth_layers += 1;
        r.layers_missing_coverage += usize::from(!layer.has_normal_coverage());
        match session.bindings.iter().filter(|b| b.enabled && b.layer_id == layer.id).count() {
            0 => r.layers_missing_target += 1,
            1 => {}
            _ => r.layers_with_ambiguous_bindings += 1,
        }
    }
    r
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayerStackRow {
    pub source_index: usize,
    pub layer_id: String,
    pub display_name: String,
    pub layer_path: String,
    pub enabled: bool,
    pub visible: bool,
    pub has_depth_pixels: bool,
    pub has_normal_coverage: bool,
    pub has_binding: bool,
    pub has_ambiguous_bindings: bool,
    pub target_grid_uuid: u64,
    pub target_display_name: String,
    pub merge_policy: MergePolicy,
    pub sampled_vertices: usize,
    pub missing_vertices: usize,
    pub winning_vertices: usize,
    pub has_depth_range: bool,
    pub min_depth: f32,
    pub max_depth: f32,
    pub warning_missing_depth_pixels: bool,
    pub warning_missing_target: bool,
    pub warning_missing_coverage: bool,
    pub warning_mostly_missing_samples: bool,
}

/// D `ngDepthDrawLayerStackRows`.
pub fn layer_stack_rows(
    session: &Session,
    compose: Option<&ComposeResult>,
    target_name: Option<&dyn Fn(u64) -> String>,
) -> Vec<LayerStackRow> {
    let mut rows = Vec::new();
    for (source_index, layer) in session.layers.iter().enumerate() {
        let mut row = LayerStackRow {
            source_index,
            layer_id: layer.id.clone(),
            display_name: if layer.display_name.is_empty() { layer.id.clone() } else { layer.display_name.clone() },
            layer_path: layer.layer_path.clone(),
            enabled: layer.enabled,
            visible: layer.visible,
            has_depth_pixels: layer.has_depth_pixels(),
            has_normal_coverage: layer.has_normal_coverage(),
            has_binding: false,
            has_ambiguous_bindings: false,
            target_grid_uuid: 0,
            target_display_name: String::new(),
            merge_policy: MergePolicy::Frontmost,
            sampled_vertices: 0,
            missing_vertices: 0,
            winning_vertices: 0,
            has_depth_range: false,
            min_depth: 0.0,
            max_depth: 0.0,
            warning_missing_depth_pixels: !layer.has_depth_pixels(),
            warning_missing_target: false,
            warning_missing_coverage: layer.has_depth_pixels() && !layer.has_normal_coverage(),
            warning_mostly_missing_samples: false,
        };
        let mut count = 0;
        for binding in session.bindings.iter().filter(|b| b.enabled && b.layer_id == layer.id) {
            count += 1;
            if count == 1 {
                row.target_grid_uuid = binding.target_grid_uuid;
                if let Some(name) = target_name {
                    row.target_display_name = name(binding.target_grid_uuid);
                }
                row.merge_policy = binding.merge_policy;
            }
        }
        row.has_binding = count > 0;
        row.has_ambiguous_bindings = count > 1;
        row.warning_missing_target = row.has_depth_pixels && !row.has_binding;
        if let Some(result) = compose {
            for stats in result.layer_stats.iter().filter(|s| s.layer_id == layer.id) {
                row.sampled_vertices += stats.sampled_vertices;
                row.missing_vertices += stats.missing_vertices;
                row.winning_vertices += stats.winning_vertices;
                if stats.has_depth_range {
                    if !row.has_depth_range {
                        row.has_depth_range = true;
                        row.min_depth = stats.min_depth;
                        row.max_depth = stats.max_depth;
                    } else {
                        row.min_depth = row.min_depth.min(stats.min_depth);
                        row.max_depth = row.max_depth.max(stats.max_depth);
                    }
                }
            }
        }
        row.warning_mostly_missing_samples = row.missing_vertices > 0 && row.missing_vertices > row.sampled_vertices;
        rows.push(row);
    }
    rows
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackSort {
    SourceOrder,
    Name,
    Target,
    DepthMin,
    Sampled,
    Missing,
}

/// D `ngDepthDrawFilterAndSortLayerStackRows`: a case-insensitive substring filter over id, name, path and
/// target, then a stable sort (ties keep source order).
pub fn filter_and_sort_rows(rows: Vec<LayerStackRow>, filter: &str, sort: StackSort, descending: bool) -> Vec<LayerStackRow> {
    let needle = filter.to_lowercase();
    let mut result: Vec<LayerStackRow> = rows
        .into_iter()
        .filter(|row| {
            needle.is_empty() || {
                let target = if row.has_binding { row.target_grid_uuid.to_string() } else { String::new() };
                let haystack =
                    format!("{}\n{}\n{}\n{}\n{}", row.layer_id, row.display_name, row.layer_path, row.target_display_name, target)
                        .to_lowercase();
                haystack.contains(&needle)
            }
        })
        .collect();
    let text = |s: &str, fallback: String| (if s.is_empty() { fallback } else { s.to_owned() }).to_lowercase();
    let order = |ord: std::cmp::Ordering| if descending { ord.reverse() } else { ord };
    match sort {
        StackSort::SourceOrder => {}
        StackSort::Name => result.sort_by(|a, b| {
            let (av, bv) = (text(&a.display_name, a.layer_id.clone()), text(&b.display_name, b.layer_id.clone()));
            if av == bv { a.source_index.cmp(&b.source_index) } else { order(av.cmp(&bv)) }
        }),
        StackSort::Target => result.sort_by(|a, b| {
            let (av, bv) = (
                text(&a.target_display_name, a.target_grid_uuid.to_string()),
                text(&b.target_display_name, b.target_grid_uuid.to_string()),
            );
            if av == bv { a.source_index.cmp(&b.source_index) } else { order(av.cmp(&bv)) }
        }),
        StackSort::DepthMin => result.sort_by(|a, b| {
            let av = if a.has_depth_range { a.min_depth } else { f32::MAX };
            let bv = if b.has_depth_range { b.min_depth } else { f32::MAX };
            if av == bv { a.source_index.cmp(&b.source_index) } else { order(av.total_cmp(&bv)) }
        }),
        StackSort::Sampled => result.sort_by(|a, b| {
            if a.sampled_vertices == b.sampled_vertices {
                a.source_index.cmp(&b.source_index)
            } else {
                order(a.sampled_vertices.cmp(&b.sampled_vertices))
            }
        }),
        StackSort::Missing => result.sort_by(|a, b| {
            if a.missing_vertices == b.missing_vertices {
                a.source_index.cmp(&b.source_index)
            } else {
                order(a.missing_vertices.cmp(&b.missing_vertices))
            }
        }),
    }
    result
}

/// D `ngDepthDrawWinningLayerColor`: a stable colour per layer id (FNV-1a), channels in 0.25..0.95.
pub fn winning_layer_color(layer_id: &str) -> [f32; 4] {
    let mut hash: u32 = 2166136261;
    for byte in layer_id.bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(16777619);
    }
    let channel = |shift: u32| 0.25 + ((hash >> shift) & 0xFF) as f32 / 255.0 * 0.7;
    [channel(0), channel(8), channel(16), 1.0]
}

#[derive(Clone, Debug, PartialEq)]
pub struct CompositePreviewPoint {
    pub vertex_index: usize,
    pub depth: f32,
    pub winning_layer_id: String,
    pub winning_layer_color: [f32; 4],
    pub sampled: bool,
    pub missing: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CompositePreviewSummary {
    pub target_grid_uuid: u64,
    pub vertex_count: usize,
    pub sampled_vertices: usize,
    pub missing_vertices: usize,
    pub winning_vertices: usize,
    pub final_sampled_vertices: usize,
    pub final_missing_vertices: usize,
    pub has_depth_range: bool,
    pub min_depth: f32,
    pub max_depth: f32,
    pub points: Vec<CompositePreviewPoint>,
}

/// D `ngDepthDrawCompositePreviewSummary`.
pub fn composite_preview_summary(result: &ComposeResult) -> CompositePreviewSummary {
    let mut summary = CompositePreviewSummary {
        target_grid_uuid: result.target_grid_uuid,
        vertex_count: result.depths.len(),
        sampled_vertices: result.sampled_vertices,
        missing_vertices: result.missing_vertices,
        has_depth_range: result.has_depth_range,
        min_depth: result.min_depth,
        max_depth: result.max_depth,
        ..Default::default()
    };
    for (i, depth) in result.depths.iter().enumerate() {
        let winner = result.winning_layer_ids.get(i).cloned().unwrap_or_default();
        let sampled = !winner.is_empty();
        if sampled {
            summary.winning_vertices += 1;
            summary.final_sampled_vertices += 1;
        } else {
            summary.final_missing_vertices += 1;
        }
        summary.points.push(CompositePreviewPoint {
            vertex_index: i,
            depth: *depth,
            winning_layer_color: if sampled { winning_layer_color(&winner) } else { [0.0; 4] },
            winning_layer_id: winner,
            sampled,
            missing: !sampled,
        });
    }
    summary
}

#[derive(Clone, Debug, PartialEq)]
pub struct DepthSpacePlane {
    pub layer_id: String,
    pub display_name: String,
    pub bounds: Rect,
    pub selected: bool,
    pub visible: bool,
    pub enabled: bool,
    pub has_depth_range: bool,
    pub min_depth: f32,
    pub max_depth: f32,
    pub representative_depth: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DepthSpaceGap {
    pub back_layer_id: String,
    pub front_layer_id: String,
    pub valid: bool,
    pub overlap: bool,
    pub back_depth: f32,
    pub front_depth: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DepthSpaceSummary {
    pub selected_grid_uuid: u64,
    pub selected_layer_id: String,
    pub layers: Vec<DepthSpacePlane>,
    pub gaps: Vec<DepthSpaceGap>,
}

/// D `ngDepthDrawDepthSpaceSummary`: one plane per layer and the gap between neighbouring layers' ranges.
pub fn depth_space_summary(session: &Session, compose: Option<&ComposeResult>) -> DepthSpaceSummary {
    let mut summary = DepthSpaceSummary {
        selected_grid_uuid: session.selected_grid_uuid,
        selected_layer_id: session.selected_layer_id.clone(),
        ..Default::default()
    };
    let rows = layer_stack_rows(session, compose, None);
    for (i, row) in rows.iter().enumerate() {
        let Some(layer) = session.layer_by_id(&row.layer_id) else { continue };
        summary.layers.push(DepthSpacePlane {
            layer_id: row.layer_id.clone(),
            display_name: row.display_name.clone(),
            bounds: layer.bounds,
            selected: row.layer_id == session.selected_layer_id,
            visible: row.visible,
            enabled: row.enabled,
            has_depth_range: row.has_depth_range,
            min_depth: row.min_depth,
            max_depth: row.max_depth,
            representative_depth: if row.has_depth_range { (row.min_depth + row.max_depth) * 0.5 } else { 0.0 },
        });
        if i == 0 {
            continue;
        }
        let previous = &rows[i - 1];
        let mut gap = DepthSpaceGap {
            back_layer_id: previous.layer_id.clone(),
            front_layer_id: row.layer_id.clone(),
            valid: false,
            overlap: false,
            back_depth: 0.0,
            front_depth: 0.0,
        };
        if previous.has_depth_range && row.has_depth_range {
            gap.back_depth = previous.max_depth;
            gap.front_depth = row.min_depth;
            gap.overlap = gap.front_depth < gap.back_depth;
            gap.valid = true;
        }
        summary.gaps.push(gap);
    }
    summary
}

// ------------------------------------------------------------------------------------- fit z

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct DepthRange {
    pub valid: bool,
    pub min_depth: f32,
    pub max_depth: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FitZResult {
    pub succeeded: bool,
    pub error: String,
    pub z_scale: f32,
    pub z_offset: f32,
    pub target_min: f32,
    pub target_max: f32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FitZGap {
    pub valid: bool,
    pub error: String,
    pub back: f32,
    pub front: f32,
}

pub fn range_from_values(values: &[f32]) -> DepthRange {
    let mut range = DepthRange::default();
    for value in values.iter().filter(|v| v.is_finite()) {
        if !range.valid {
            range = DepthRange { valid: true, min_depth: *value, max_depth: *value };
        } else {
            range.min_depth = range.min_depth.min(*value);
            range.max_depth = range.max_depth.max(*value);
        }
    }
    range
}

fn gap_error(error: &str) -> FitZGap {
    FitZGap { error: error.into(), ..Default::default() }
}

/// D `ngDepthDrawGapFromAdjacentRanges`: the gap between the layers before and after `layer_index`.
pub fn gap_from_adjacent_ranges(ranges: &[DepthRange], layer_index: usize) -> FitZGap {
    if layer_index == 0 || layer_index + 1 >= ranges.len() {
        return gap_error("adjacent layers are required");
    }
    let (back, front) = (ranges[layer_index - 1], ranges[layer_index + 1]);
    if !back.valid {
        return gap_error("back adjacent layer has no usable range");
    }
    if !front.valid {
        return gap_error("front adjacent layer has no usable range");
    }
    if front.min_depth < back.max_depth {
        return FitZGap { error: "adjacent layer ranges overlap".into(), back: back.max_depth, front: front.min_depth, valid: false };
    }
    FitZGap { valid: true, error: String::new(), back: back.max_depth, front: front.min_depth }
}

pub fn gap_from_target_range(range: DepthRange) -> FitZGap {
    if !range.valid {
        return gap_error("target has no usable depth range");
    }
    FitZGap { valid: true, error: String::new(), back: range.min_depth, front: range.max_depth }
}

pub fn gap_from_selected_layer_ranges(back: DepthRange, front: DepthRange) -> FitZGap {
    if !back.valid {
        return gap_error("selected back layer has no usable range");
    }
    if !front.valid {
        return gap_error("selected front layer has no usable range");
    }
    if front.min_depth < back.max_depth {
        return FitZGap { error: "selected layer ranges overlap".into(), back: back.max_depth, front: front.min_depth, valid: false };
    }
    FitZGap { valid: true, error: String::new(), back: back.max_depth, front: front.min_depth }
}

/// D `ngDepthDrawFitZToGap`: the z scale and offset that map the raw range into `[back + margin, front - margin]`.
pub fn fit_z_to_gap(raw: DepthRange, gap_back: f32, gap_front: f32, margin: f32) -> FitZResult {
    let mut result = FitZResult::default();
    if !raw.valid {
        result.error = "raw range has no usable samples".into();
        return result;
    }
    if !gap_back.is_finite() || !gap_front.is_finite() || !margin.is_finite() {
        result.error = "gap values must be finite".into();
        return result;
    }
    result.target_min = gap_back + margin;
    result.target_max = gap_front - margin;
    if result.target_max <= result.target_min {
        result.error = "gap is empty after margin".into();
        return result;
    }
    let span = raw.max_depth - raw.min_depth;
    if span == 0.0 {
        result.z_scale = 1.0;
        result.z_offset = ((result.target_min + result.target_max) * 0.5) - raw.min_depth;
    } else {
        result.z_scale = (result.target_max - result.target_min) / span;
        result.z_offset = result.target_min - raw.min_depth * result.z_scale;
    }
    result.succeeded = true;
    result
}

pub fn fit_z_to_gap_checked(raw: DepthRange, gap: &FitZGap, margin: f32) -> FitZResult {
    if !gap.valid {
        return FitZResult {
            error: if gap.error.is_empty() { "gap is invalid".into() } else { gap.error.clone() },
            ..Default::default()
        };
    }
    fit_z_to_gap(raw, gap.back, gap.front, margin)
}

/// D `ngDepthDrawApplyFitZToGap`: fit and, when it works, store the transform on the layer.
pub fn apply_fit_z_to_gap(layer: &mut Layer, raw: DepthRange, gap_back: f32, gap_front: f32, margin: f32) -> FitZResult {
    let result = fit_z_to_gap(raw, gap_back, gap_front, margin);
    if result.succeeded {
        layer.z_scale = result.z_scale;
        layer.z_offset = result.z_offset;
    }
    result
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FitZDiagnostics {
    pub raw_range_valid: bool,
    pub raw_min: f32,
    pub raw_max: f32,
    pub gap_valid: bool,
    pub gap_back: f32,
    pub gap_front: f32,
    pub target_min: f32,
    pub target_max: f32,
    pub z_scale: f32,
    pub z_offset: f32,
    pub succeeded: bool,
    pub error: String,
}

pub fn fit_z_diagnostics(raw: DepthRange, gap: &FitZGap, fit: &FitZResult) -> FitZDiagnostics {
    FitZDiagnostics {
        raw_range_valid: raw.valid,
        raw_min: raw.min_depth,
        raw_max: raw.max_depth,
        gap_valid: gap.valid,
        gap_back: gap.back,
        gap_front: gap.front,
        target_min: fit.target_min,
        target_max: fit.target_max,
        z_scale: fit.z_scale,
        z_offset: fit.z_offset,
        succeeded: fit.succeeded,
        error: if fit.error.is_empty() { gap.error.clone() } else { fit.error.clone() },
    }
}


// ----------------------------------------------------------------------------- readback comparison

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadbackComparison {
    pub ok: bool,
    pub error: String,
    pub depth_mismatches: usize,
    pub winner_mismatches: usize,
    pub missing_flag_mismatches: usize,
    pub target_mismatches: usize,
    pub layer_stat_mismatches: usize,
    pub aggregate_stat_mismatches: usize,
}

impl Default for ReadbackComparison {
    fn default() -> Self {
        Self {
            ok: true,
            error: String::new(),
            depth_mismatches: 0,
            winner_mismatches: 0,
            missing_flag_mismatches: 0,
            target_mismatches: 0,
            layer_stat_mismatches: 0,
            aggregate_stat_mismatches: 0,
        }
    }
}

impl ReadbackComparison {
    fn fail(&mut self, error: &str) {
        if self.ok {
            self.ok = false;
            self.error = error.to_owned();
        }
    }
}

fn same_depth(a: f32, b: f32, tolerance: f32) -> bool {
    if a.is_nan() || b.is_nan() {
        return a.is_nan() && b.is_nan();
    }
    (a - b).abs() <= tolerance
}

/// D `ngCompareDepthDrawReadback`: two depth/winner arrays must agree within `tolerance`.
pub fn compare_readback(
    cpu_depths: &[f32],
    cpu_winners: &[String],
    readback_depths: &[f32],
    readback_winners: &[String],
    tolerance: f32,
) -> ReadbackComparison {
    let mut result = ReadbackComparison::default();
    if !tolerance.is_finite() || tolerance < 0.0 {
        result.fail("DepthDraw readback tolerance must be finite and non-negative");
        return result;
    }
    if cpu_depths.len() != readback_depths.len() {
        result.fail("DepthDraw readback depth length mismatch");
        return result;
    }
    if cpu_winners.len() != readback_winners.len() {
        result.fail("DepthDraw readback winner length mismatch");
        return result;
    }
    if cpu_depths.len() != cpu_winners.len() {
        result.fail("DepthDraw CPU depth/winner length mismatch");
        return result;
    }
    for (i, cpu_depth) in cpu_depths.iter().enumerate() {
        if !same_depth(*cpu_depth, readback_depths[i], tolerance) {
            result.depth_mismatches += 1;
            result.fail("DepthDraw readback depth mismatch");
        }
        if cpu_winners[i].is_empty() != readback_winners[i].is_empty() {
            result.missing_flag_mismatches += 1;
            result.fail("DepthDraw readback missing flag mismatch");
        }
        if cpu_winners[i] != readback_winners[i] {
            result.winner_mismatches += 1;
            result.fail("DepthDraw readback winner mismatch");
        }
    }
    result
}

/// D `ngCompareDepthDrawComposeReadback`: arrays, target, aggregate stats and per-layer stats.
pub fn compare_compose_readback(cpu: &ComposeResult, readback: &ComposeResult, tolerance: f32) -> ReadbackComparison {
    let mut result =
        compare_readback(&cpu.depths, &cpu.winning_layer_ids, &readback.depths, &readback.winning_layer_ids, tolerance);
    if cpu.target_grid_uuid != readback.target_grid_uuid {
        result.target_mismatches += 1;
        result.fail("DepthDraw readback target GridDeformer mismatch");
    }
    if cpu.sampled_vertices != readback.sampled_vertices
        || cpu.missing_vertices != readback.missing_vertices
        || cpu.has_depth_range != readback.has_depth_range
        || (cpu.has_depth_range
            && (!same_depth(cpu.min_depth, readback.min_depth, tolerance)
                || !same_depth(cpu.max_depth, readback.max_depth, tolerance)))
    {
        result.aggregate_stat_mismatches += 1;
        result.fail("DepthDraw readback aggregate stats mismatch");
    }
    if cpu.layer_stats.len() != readback.layer_stats.len() {
        result.layer_stat_mismatches += 1;
        result.fail("DepthDraw readback layer stats length mismatch");
        return result;
    }
    for (a, b) in cpu.layer_stats.iter().zip(&readback.layer_stats) {
        if a.layer_id != b.layer_id
            || a.sampled_vertices != b.sampled_vertices
            || a.missing_vertices != b.missing_vertices
            || a.winning_vertices != b.winning_vertices
            || a.contributed_vertices != b.contributed_vertices
            || a.additive_merge != b.additive_merge
            || a.has_depth_range != b.has_depth_range
            || (a.has_depth_range
                && (!same_depth(a.min_depth, b.min_depth, tolerance) || !same_depth(a.max_depth, b.max_depth, tolerance)))
        {
            result.layer_stat_mismatches += 1;
            result.fail("DepthDraw readback layer stats mismatch");
        }
    }
    result
}

// ------------------------------------------------------------------------------------- manifest

fn number_field(object: &Map<String, Value>, key: &str, name: &str, fallback: f64) -> Result<f64, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(fallback),
        Some(value) => value.as_f64().ok_or_else(|| format!("{name} must be a number")),
    }
}

fn bool_field(object: &Map<String, Value>, key: &str, name: &str, fallback: bool) -> Result<bool, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(fallback),
        Some(value) => value.as_bool().ok_or_else(|| format!("{name} must be a bool")),
    }
}

fn string_field(object: &Map<String, Value>, key: &str, name: &str, fallback: &str) -> Result<String, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(fallback.to_owned()),
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(format!("{name} must be a string")),
    }
}

fn pair_field(object: &Map<String, Value>, key: &str, name: &str, fallback: [f32; 2]) -> Result<[f32; 2], String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(fallback),
        Some(Value::Array(items)) if items.len() == 2 => {
            let get = |i: usize| items[i].as_f64().map(|v| v as f32).ok_or_else(|| format!("{name}[{i}] must be a number"));
            Ok([get(0)?, get(1)?])
        }
        Some(_) => Err(format!("{name} must be [x, y]")),
    }
}

fn uuid_field(object: &Map<String, Value>, key: &str, name: &str) -> Result<u64, String> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(0),
        Some(Value::String(s)) => s.parse::<u64>().map_err(|_| format!("{name} must be an unsigned integer string")),
        Some(_) => Err(format!("{name} must be a string")),
    }
}

fn bounded_int(object: &Map<String, Value>, key: &str, name: &str, min: i32, max: i32) -> Result<i32, String> {
    let number = number_field(object, key, name, 0.0)? as f32;
    Ok(if number <= min as f32 {
        min
    } else if number >= max as f32 {
        max
    } else {
        number as i32
    })
}

fn display_to_json(d: &DisplayOptions) -> Value {
    json!({
        "showNormalImage": d.show_normal_image, "showRawDepth": d.show_raw_depth, "showCoverage": d.show_coverage,
        "showComposite": d.show_composite, "showLayerPlanes": d.show_layer_planes,
        "showDepthRanges": d.show_depth_ranges, "showMissingVertices": d.show_missing_vertices,
        "showWinningLayer": d.show_winning_layer, "useGpuPreview": d.use_gpu_preview,
    })
}

fn display_from_json(value: Option<&Value>) -> Result<DisplayOptions, String> {
    let mut d = DisplayOptions::default();
    let object = match value {
        None | Some(Value::Null) => return Ok(d),
        Some(Value::Object(o)) => o,
        Some(_) => return Err("display must be an object".into()),
    };
    d.show_normal_image = bool_field(object, "showNormalImage", "display.showNormalImage", d.show_normal_image)?;
    d.show_raw_depth = bool_field(object, "showRawDepth", "display.showRawDepth", d.show_raw_depth)?;
    d.show_coverage = bool_field(object, "showCoverage", "display.showCoverage", d.show_coverage)?;
    d.show_composite = bool_field(object, "showComposite", "display.showComposite", d.show_composite)?;
    d.show_layer_planes = bool_field(object, "showLayerPlanes", "display.showLayerPlanes", d.show_layer_planes)?;
    d.show_depth_ranges = bool_field(object, "showDepthRanges", "display.showDepthRanges", d.show_depth_ranges)?;
    d.show_missing_vertices =
        bool_field(object, "showMissingVertices", "display.showMissingVertices", d.show_missing_vertices)?;
    d.show_winning_layer = bool_field(object, "showWinningLayer", "display.showWinningLayer", d.show_winning_layer)?;
    d.use_gpu_preview = bool_field(object, "useGpuPreview", "display.useGpuPreview", d.use_gpu_preview)?;
    Ok(d)
}

fn cleanup_to_json(operations: &[CleanupOperation]) -> Value {
    Value::Array(
        operations
            .iter()
            .map(|op| {
                let rules: Vec<Value> = op
                    .focused_rules
                    .iter()
                    .map(|r| {
                        json!({"layerIndex": r.layer_index, "x": r.x, "y": r.y, "w": r.w, "h": r.h,
                               "lift": r.lift, "radius": r.radius})
                    })
                    .collect();
                json!({
                    "kind": match op.kind {
                        CleanupKind::AlphaDepthGapFill => "AlphaDepthGapFill",
                        CleanupKind::ContourRepair => "ContourRepair",
                    },
                    "contourThickness": op.contour_thickness,
                    "focusedRules": rules,
                })
            })
            .collect(),
    )
}

fn cleanup_from_json(value: Option<&Value>, name: &str, width: i32, height: i32) -> Result<Vec<CleanupOperation>, String> {
    let items = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err(format!("{name} must be an array")),
    };
    let mut result = Vec::new();
    for (i, entry) in items.iter().enumerate() {
        let item = format!("{name}[{i}]");
        let object = entry.as_object().ok_or_else(|| format!("{item} must be an object"))?;
        let kind = match string_field(object, "kind", &format!("{item}.kind"), "")?.as_str() {
            "AlphaDepthGapFill" => CleanupKind::AlphaDepthGapFill,
            "ContourRepair" => CleanupKind::ContourRepair,
            other => return Err(format!("Unknown DepthDraw cleanup operation: {other}")),
        };
        let thickness = number_field(object, "contourThickness", &format!("{item}.contourThickness"), 2.0)? as f32;
        let contour_thickness = if thickness >= MAX_CONTOUR_THICKNESS as f32 {
            MAX_CONTOUR_THICKNESS
        } else {
            normalize_contour_thickness(thickness as i32)
        };
        let rules_value = object.get("focusedRules").cloned().unwrap_or(Value::Array(Vec::new()));
        let rules_items = rules_value.as_array().ok_or_else(|| format!("{item}.focusedRules must be an array"))?;
        let mut focused_rules = Vec::new();
        for (j, rule_value) in rules_items.iter().enumerate() {
            let rname = format!("{item}.focusedRules[{j}]");
            let rule_object = rule_value.as_object().ok_or_else(|| format!("{rname} must be an object"))?;
            let max_x = if width > 0 { width } else { i32::MAX };
            let max_y = if height > 0 { height } else { i32::MAX };
            let mut rule = FocusedRule {
                layer_index: bounded_int(rule_object, "layerIndex", &format!("{rname}.layerIndex"), 0, i32::MAX)?,
                x: bounded_int(rule_object, "x", &format!("{rname}.x"), 0, max_x)?,
                y: bounded_int(rule_object, "y", &format!("{rname}.y"), 0, max_y)?,
                w: bounded_int(rule_object, "w", &format!("{rname}.w"), 0, max_x)?,
                h: bounded_int(rule_object, "h", &format!("{rname}.h"), 0, max_y)?,
                lift: bounded_int(rule_object, "lift", &format!("{rname}.lift"), 0, 255)?,
                radius: bounded_int(rule_object, "radius", &format!("{rname}.radius"), 0, MAX_FOCUSED_RULE_RADIUS)?,
            };
            if width > 0 && height > 0 {
                rule = normalize_focused_rule(rule, width, height);
            }
            focused_rules.push(rule);
        }
        result.push(CleanupOperation { kind, contour_thickness, focused_rules });
    }
    Ok(result)
}

/// D `ngDepthDrawSessionToManifest`: the persisted form (version 1). Pixels are not stored; layers
/// reference their source file.
pub fn session_to_manifest(session: &Session) -> Value {
    let layers: Vec<Value> = session
        .layers
        .iter()
        .map(|l| {
            json!({
                "id": l.id, "sourcePath": l.source_path, "layerPath": l.layer_path, "displayName": l.display_name,
                "width": l.width, "height": l.height,
                "bounds": [l.bounds.left, l.bounds.top, l.bounds.width, l.bounds.height],
                "opacity": f64::from(l.opacity), "visible": l.visible, "enabled": l.enabled,
                "xyOffset": [f64::from(l.xy_offset[0]), f64::from(l.xy_offset[1])],
                "xyScale": [f64::from(l.xy_scale[0]), f64::from(l.xy_scale[1])],
                "zOffset": f64::from(l.z_offset), "zScale": f64::from(l.z_scale),
                "backDepth": f64::from(l.back_depth), "frontDepth": f64::from(l.front_depth),
                "sampleDepthScale": f64::from(l.sample_depth_scale), "invert": l.invert,
                "channel": l.channel.name(), "convolution": l.convolution.name(),
                "customRadius": l.custom_radius, "alphaThreshold": f64::from(l.alpha_threshold),
                "cleanupOperations": cleanup_to_json(&l.cleanup_operations),
            })
        })
        .collect();
    let bindings: Vec<Value> = session
        .bindings
        .iter()
        .map(|b| {
            json!({
                "layerId": b.layer_id, "targetNodeUuid": b.target_node_uuid.to_string(),
                "targetGridUuid": b.target_grid_uuid.to_string(), "order": b.order, "enabled": b.enabled,
                "useNormalLayerAlpha": b.use_normal_layer_alpha, "coverageThreshold": f64::from(b.coverage_threshold),
                "mergePolicy": b.merge_policy.name(),
            })
        })
        .collect();
    json!({
        "version": 1,
        "selectedLayerId": session.selected_layer_id,
        "selectedGridUuid": session.selected_grid_uuid.to_string(),
        "display": display_to_json(&session.display),
        "sourceIdentity": session.source_identity,
        "documentWidth": session.document_width,
        "documentHeight": session.document_height,
        "layers": layers,
        "bindings": bindings,
    })
}

/// D `ngDepthDrawSessionFromManifest`. Malformed fields are an error, never a panic.
pub fn session_from_manifest(manifest: &Value) -> Result<Session, String> {
    let root = manifest.as_object().ok_or("DepthDraw manifest must be an object")?;
    let mut session = Session::new();
    session.selected_layer_id = string_field(root, "selectedLayerId", "selectedLayerId", "")?;
    session.selected_grid_uuid = uuid_field(root, "selectedGridUuid", "selectedGridUuid")?;
    session.display = display_from_json(root.get("display"))?;
    session.source_identity = string_field(root, "sourceIdentity", "sourceIdentity", "")?;
    session.document_width = number_field(root, "documentWidth", "documentWidth", 0.0)? as i32;
    session.document_height = number_field(root, "documentHeight", "documentHeight", 0.0)? as i32;
    let layers = match root.get("layers") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Err("DepthDraw manifest layers must be an array".into()),
    };
    for entry in &layers {
        let o = entry.as_object().ok_or("DepthDraw manifest layer must be an object")?;
        let mut layer = Layer {
            id: string_field(o, "id", "layer.id", "")?,
            source_path: string_field(o, "sourcePath", "layer.sourcePath", "")?,
            layer_path: string_field(o, "layerPath", "layer.layerPath", "")?,
            display_name: string_field(o, "displayName", "layer.displayName", "")?,
            width: number_field(o, "width", "layer.width", 0.0)? as i32,
            height: number_field(o, "height", "layer.height", 0.0)? as i32,
            ..Layer::default()
        };
        layer.bounds = match o.get("bounds") {
            None | Some(Value::Null) => Rect { left: 0, top: 0, width: layer.width, height: layer.height },
            Some(Value::Array(items)) if items.len() == 4 => {
                let get = |i: usize| items[i].as_f64().map(|v| v as f32 as i32).ok_or("layer.bounds must be numbers");
                Rect { left: get(0)?, top: get(1)?, width: get(2)?, height: get(3)? }
            }
            Some(_) => return Err("layer.bounds must be [left, top, width, height]".into()),
        };
        layer.opacity = number_field(o, "opacity", "layer.opacity", 1.0)? as f32;
        layer.visible = bool_field(o, "visible", "layer.visible", true)?;
        layer.enabled = bool_field(o, "enabled", "layer.enabled", true)?;
        layer.xy_offset = pair_field(o, "xyOffset", "layer.xyOffset", [0.0, 0.0])?;
        layer.xy_scale = pair_field(o, "xyScale", "layer.xyScale", [1.0, 1.0])?;
        layer.z_offset = number_field(o, "zOffset", "layer.zOffset", 0.0)? as f32;
        layer.z_scale = number_field(o, "zScale", "layer.zScale", 1.0)? as f32;
        layer.back_depth = number_field(o, "backDepth", "layer.backDepth", -1.0)? as f32;
        layer.front_depth = number_field(o, "frontDepth", "layer.frontDepth", 1.0)? as f32;
        layer.sample_depth_scale = number_field(o, "sampleDepthScale", "layer.sampleDepthScale", 1.0)? as f32;
        layer.invert = bool_field(o, "invert", "layer.invert", false)?;
        layer.channel = Channel::from_name(&string_field(o, "channel", "layer.channel", "AverageRGB")?)?;
        layer.convolution = Convolution::from_name(&string_field(o, "convolution", "layer.convolution", "Gaussian3x3")?)?;
        layer.custom_radius = normalize_custom_radius(number_field(o, "customRadius", "layer.customRadius", 3.0)? as i32);
        layer.alpha_threshold = number_field(o, "alphaThreshold", "layer.alphaThreshold", 0.01)? as f32;
        layer.cleanup_operations =
            cleanup_from_json(o.get("cleanupOperations"), "layer.cleanupOperations", layer.width, layer.height)?;
        session.layers.push(layer);
    }
    let bindings = match root.get("bindings") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.clone(),
        Some(_) => return Err("DepthDraw manifest bindings must be an array".into()),
    };
    for entry in &bindings {
        let o = entry.as_object().ok_or("DepthDraw manifest binding must be an object")?;
        session.bindings.push(Binding {
            layer_id: string_field(o, "layerId", "binding.layerId", "")?,
            target_node_uuid: uuid_field(o, "targetNodeUuid", "binding.targetNodeUuid")?,
            target_grid_uuid: uuid_field(o, "targetGridUuid", "binding.targetGridUuid")?,
            order: number_field(o, "order", "binding.order", 0.0)? as i32,
            enabled: bool_field(o, "enabled", "binding.enabled", true)?,
            use_normal_layer_alpha: bool_field(o, "useNormalLayerAlpha", "binding.useNormalLayerAlpha", true)?,
            coverage_threshold: number_field(o, "coverageThreshold", "binding.coverageThreshold", 0.01)? as f32,
            merge_policy: MergePolicy::from_name(&string_field(o, "mergePolicy", "binding.mergePolicy", "Frontmost")?)?,
        });
    }
    session.normalize_gpu_preview_sampling();
    Ok(session)
}

/// D `ngDepthDrawSessionToPersistentJson`.
pub fn session_to_persistent_json(session: &Session) -> String {
    session_to_manifest(session).to_string()
}

/// D `ngDepthDrawSessionFromPersistentJson`: an empty string means "no session".
pub fn session_from_persistent_json(text: &str) -> Result<Option<Session>, String> {
    if text.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(text).map_err(|e| format!("DepthDraw manifest is not valid JSON: {e}"))?;
    session_from_manifest(&value).map(Some)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ManifestValidation {
    pub missing_source_layer_ids: Vec<String>,
    pub missing_target_layer_ids: Vec<String>,
    pub missing_binding_layer_ids: Vec<String>,
    pub duplicate_layer_ids: Vec<String>,
    pub duplicate_binding_keys: Vec<String>,
    pub missing_selected_layer_id: String,
    pub missing_selected_grid_uuid: u64,
}

impl ManifestValidation {
    pub fn ok(&self) -> bool {
        self.missing_source_layer_ids.is_empty()
            && self.missing_target_layer_ids.is_empty()
            && self.missing_binding_layer_ids.is_empty()
            && self.duplicate_layer_ids.is_empty()
            && self.duplicate_binding_keys.is_empty()
            && self.missing_selected_layer_id.is_empty()
            && self.missing_selected_grid_uuid == 0
    }
}

/// D `ngValidateDepthDrawSessionManifest`. `source_exists` answers for a layer's `sourcePath` (the caller
/// resolves it against the manifest folder); `has_target_grid` answers for a grid uuid in the document.
pub fn validate_manifest(
    session: &Session,
    has_target_grid: Option<&dyn Fn(u64) -> bool>,
    source_exists: &dyn Fn(&str) -> bool,
) -> ManifestValidation {
    let mut result = ManifestValidation::default();
    let mut seen: Vec<&str> = Vec::new();
    for layer in &session.layers {
        if !layer.id.is_empty() {
            if seen.contains(&layer.id.as_str()) {
                if !result.duplicate_layer_ids.contains(&layer.id) {
                    result.duplicate_layer_ids.push(layer.id.clone());
                }
            } else {
                seen.push(&layer.id);
            }
        }
        if !layer.source_path.is_empty() && !source_exists(&layer.source_path) {
            result.missing_source_layer_ids.push(layer.id.clone());
        }
    }
    if !session.selected_layer_id.is_empty() && session.layer_by_id(&session.selected_layer_id).is_none() {
        result.missing_selected_layer_id = session.selected_layer_id.clone();
    }
    let mut seen_keys: Vec<String> = Vec::new();
    for binding in session.bindings.iter().filter(|b| b.enabled) {
        let key = format!("{}\0{}", binding.layer_id, binding.target_grid_uuid);
        if seen_keys.contains(&key) {
            if !result.duplicate_binding_keys.contains(&key) {
                result.duplicate_binding_keys.push(key);
            }
        } else {
            seen_keys.push(key);
        }
        if binding.layer_id.is_empty() || session.layer_by_id(&binding.layer_id).is_none() {
            result.missing_binding_layer_ids.push(binding.layer_id.clone());
        }
        if binding.target_grid_uuid == 0 {
            result.missing_target_layer_ids.push(binding.layer_id.clone());
            continue;
        }
        if has_target_grid.is_some_and(|f| !f(binding.target_grid_uuid)) {
            result.missing_target_layer_ids.push(binding.layer_id.clone());
        }
    }
    if session.selected_grid_uuid != 0
        && (!session.has_target_grid(session.selected_grid_uuid)
            || has_target_grid.is_some_and(|f| !f(session.selected_grid_uuid)))
    {
        result.missing_selected_grid_uuid = session.selected_grid_uuid;
    }
    result
}

// ------------------------------------------------------------------------------------- png export

/// D `ngDepthDrawLayerToExportRgba`: grey depth (decoded through the layer channel) with the source alpha.
pub fn layer_to_export_rgba(layer: &Layer) -> Result<Vec<u8>, String> {
    if layer.width <= 0 || layer.height <= 0 {
        return Err("DepthDraw layer dimensions must be positive".into());
    }
    if !layer.has_depth_pixels() {
        return Err("DepthDraw layer must have RGBA depth pixels".into());
    }
    let depth = decode_depth_pixels(&layer.depth_pixels, layer.channel)?;
    let count = layer.width as usize * layer.height as usize;
    let mut out = vec![0u8; count * 4];
    for i in 0..count {
        out[i * 4..i * 4 + 3].fill(depth[i]);
        out[i * 4 + 3] = layer.depth_pixels[i * 4 + 3];
    }
    Ok(out)
}

/// PNG bytes of [`layer_to_export_rgba`] (D `ngSaveDepthDrawLayerPng` without the file write).
pub fn layer_png_bytes(layer: &Layer) -> Result<Vec<u8>, String> {
    let rgba = layer_to_export_rgba(layer)?;
    let image = image::RgbaImage::from_raw(layer.width as u32, layer.height as u32, rgba)
        .ok_or("DepthDraw PNG export failed: pixel buffer size")?;
    let mut bytes = std::io::Cursor::new(Vec::new());
    image
        .write_to(&mut bytes, image::ImageFormat::Png)
        .map_err(|e| format!("DepthDraw PNG export failed: {e}"))?;
    Ok(bytes.into_inner())
}

/// D `safeLayerFileName`: id (else display name, else `layer-N`) with anything but `[A-Za-z0-9_-]` as `_`.
pub fn safe_layer_file_name(layer: &Layer, index: usize) -> String {
    let source = if !layer.id.is_empty() {
        layer.id.clone()
    } else if !layer.display_name.is_empty() {
        layer.display_name.clone()
    } else {
        format!("layer-{index}")
    };
    let safe: String =
        source.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    if safe.is_empty() {
        format!("layer-{index}")
    } else {
        safe
    }
}

#[derive(Clone, Debug, Default)]
pub struct PngExport {
    /// The manifest JSON of the exported session (sources point at the PNG files, pixels and cleanup dropped).
    pub manifest: Value,
    /// `(file name, PNG bytes)`, in layer order; the caller writes them next to the manifest.
    pub files: Vec<(String, Vec<u8>)>,
    pub exported_layers: usize,
    pub skipped_layers: usize,
}

/// D `ngExportDepthDrawPngSession` without file IO: one `NNN-id.png` per layer that has depth pixels.
/// Cleanup is baked into the PNG, so the exported layer carries no cleanup operations.
pub fn export_png_session(session: &Session) -> Result<PngExport, String> {
    let mut exported = Session::new();
    exported.selected_layer_id = session.selected_layer_id.clone();
    exported.selected_grid_uuid = session.selected_grid_uuid;
    exported.display = session.display;
    exported.bindings = session.bindings.clone();
    exported.document_width = session.document_width;
    exported.document_height = session.document_height;
    let mut result = PngExport::default();
    for (i, layer) in session.layers.iter().enumerate() {
        let mut copy = layer.clone();
        if !layer.has_depth_pixels() {
            result.skipped_layers += 1;
            exported.layers.push(copy);
            continue;
        }
        let name = format!("{i:03}-{}.png", safe_layer_file_name(layer, i));
        result.files.push((name.clone(), layer_png_bytes(layer)?));
        copy.source_path = name;
        copy.rgba.clear();
        copy.depth_pixels.clear();
        copy.alpha_mask.clear();
        copy.normal_coverage.clear();
        copy.cleanup_operations.clear();
        exported.layers.push(copy);
        result.exported_layers += 1;
    }
    result.manifest = session_to_manifest(&exported);
    Ok(result)
}


// ------------------------------------------------------------------------------------- tool maths

/// D `DepthCamera3D`: yaw/pitch in radians, uniform zoom, pan in view units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DepthCamera {
    pub yaw: f32,
    pub pitch: f32,
    pub zoom: f32,
    pub pan: [f32; 2],
}

impl Default for DepthCamera {
    fn default() -> Self {
        Self { yaw: 0.0, pitch: 0.0, zoom: 1.0, pan: [0.0, 0.0] }
    }
}

impl DepthCamera {
    /// Right-drag orbit (`updateDepthCamera3D`): yaw follows x, pitch follows y and stops at +-1.35 rad.
    pub fn rotate(&mut self, dx: f32, dy: f32) {
        self.yaw -= dx * 0.01;
        self.pitch = (self.pitch + dy * 0.01).clamp(-1.35, 1.35);
    }

    /// Wheel zoom: 8% per notch, kept in 0.1..8.
    pub fn zoom_by(&mut self, wheel: f32) {
        if wheel != 0.0 {
            self.zoom = (self.zoom * (1.0 + wheel * 0.08)).clamp(0.1, 8.0);
        }
    }

    pub fn pan_by(&mut self, dx: f32, dy: f32) {
        self.pan = [self.pan[0] + dx, self.pan[1] + dy];
    }
}

/// D `projectDepthPoint`.
pub fn project_depth_point(point: [f32; 2], depth: f32, cam: &DepthCamera) -> [f32; 2] {
    let (cy, sy) = (cam.yaw.cos(), cam.yaw.sin());
    let (cp, sp) = (cam.pitch.cos(), cam.pitch.sin());
    let rx = point[0] * cy + depth * sy;
    let rz = -point[0] * sy + depth * cy;
    let ry = point[1] * cp - rz * sp;
    [rx * cam.zoom + cam.pan[0], ry * cam.zoom + cam.pan[1]]
}

/// D `unprojectDepthPoint`: the model point that projects to `point` at `depth` (cos kept off zero).
pub fn unproject_depth_point(point: [f32; 2], depth: f32, cam: &DepthCamera) -> [f32; 2] {
    let (mut cy, sy) = (cam.yaw.cos(), cam.yaw.sin());
    let (mut cp, sp) = (cam.pitch.cos(), cam.pitch.sin());
    if cy.abs() < 0.0001 {
        cy = if cy < 0.0 { -0.0001 } else { 0.0001 };
    }
    if cp.abs() < 0.0001 {
        cp = if cp < 0.0 { -0.0001 } else { 0.0001 };
    }
    let nx = (point[0] - cam.pan[0]) / cam.zoom;
    let ny = (point[1] - cam.pan[1]) / cam.zoom;
    let x = (nx - depth * sy) / cy;
    let rz = -x * sy + depth * cy;
    [x, (ny + rz * sp) / cp]
}

pub const DISPLAY_PLANE_SIZE: f32 = 2.9;
pub const DISPLAY_Z_SCALE: f32 = 0.42;

/// D `ngDepthDisplayScaleForBounds`: how many view units one unit of depth is drawn as.
pub fn depth_display_scale_for_bounds(min: [f32; 2], max: [f32; 2]) -> f32 {
    let size = [max[0] - min[0], max[1] - min[1]];
    1.0f32.max(size[0].max(size[1]) * (DISPLAY_Z_SCALE / DISPLAY_PLANE_SIZE))
}

/// D `ngDepthTargetRoundDepth`: depths are kept to 1/1000.
pub fn round_depth(value: f32) -> f32 {
    (value * 1000.0).round() / 1000.0
}

/// D `ngDepthTargetClampDepth`: rounded, and 0 for a value that is not finite.
pub fn clamp_depth(value: f32) -> f32 {
    if value.is_finite() {
        round_depth(value)
    } else {
        0.0
    }
}

/// D `DepthBrushSettings`: the shared parameters of the four depth tools.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushSettings {
    pub snap_to_grid: bool,
    pub amount: f32,
    pub radius_x: f32,
    pub radius_y: f32,
    pub angle: f32,
    pub hardness: f32,
    pub flatten_strength: f32,
}

impl Default for BrushSettings {
    fn default() -> Self {
        Self {
            snap_to_grid: true,
            amount: 0.18,
            radius_x: 58.0,
            radius_y: 42.0,
            angle: 0.0,
            hardness: 1.85,
            flatten_strength: 0.75,
        }
    }
}

/// The vertex nearest to `point` (first one on ties), or `None` for an empty list or when it is
/// farther than `radius`. D `nearestLocalVertexIndex` / `nearestProjectedVertex`.
pub fn nearest_vertex(points: &[[f32; 2]], point: [f32; 2], radius: f32) -> Option<usize> {
    let mut best: Option<(usize, f32)> = None;
    for (i, p) in points.iter().enumerate() {
        let d = ((p[0] - point[0]).powi(2) + (p[1] - point[1]).powi(2)).sqrt();
        if d < radius && best.map_or(true, |(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

/// D `snapLocalPoint`: the nearest vertex position (the point itself for an empty list).
pub fn snap_to_vertex(points: &[[f32; 2]], point: [f32; 2]) -> [f32; 2] {
    nearest_vertex(points, point, f32::MAX).map_or(point, |i| points[i])
}

/// Vertices whose position lies in the rectangle spanned by two corners (D `projectedVerticesInRect`).
pub fn vertices_in_rect(points: &[[f32; 2]], a: [f32; 2], b: [f32; 2]) -> Vec<usize> {
    let (x0, x1) = (a[0].min(b[0]), a[0].max(b[0]));
    let (y0, y1) = (a[1].min(b[1]), a[1].max(b[1]));
    points.iter().enumerate().filter(|(_, p)| p[0] >= x0 && p[0] <= x1 && p[1] >= y0 && p[1] <= y1).map(|(i, _)| i).collect()
}

/// What a press on a vertex does to the selection (D `applyClickSelection`). Returns the vertex to
/// select alone on a release without drag (a press on an already selected vertex keeps the group).
pub fn click_selection(selection: &mut Vec<usize>, index: usize, ctrl: bool, shift: bool) -> Option<usize> {
    if ctrl && !shift {
        selection.retain(|i| *i != index);
    } else if shift {
        if let Some(pos) = selection.iter().position(|i| *i == index) {
            selection.remove(pos);
        } else {
            selection.push(index);
        }
    } else if !selection.contains(&index) {
        selection.clear();
        selection.push(index);
    } else {
        return Some(index);
    }
    None
}

/// D `visibleDepthDelta`: the depth change for a pointer move, along the on-screen direction of +1 depth.
/// `axis_view` is the view-space vector of one depth unit (zero in the top view, which makes vertical drags
/// work as `-dy * 0.006`).
pub fn visible_depth_delta(current: [f32; 2], origin: [f32; 2], axis_view: [f32; 2]) -> f32 {
    let denom = axis_view[0] * axis_view[0] + axis_view[1] * axis_view[1];
    if denom > 0.000001 {
        return ((current[0] - origin[0]) * axis_view[0] + (current[1] - origin[1]) * axis_view[1]) / denom;
    }
    -(current[1] - origin[1]) * 0.006
}

/// The working depths while the Direct Depth drag is active: the dragged vertices at `start + delta`
/// (rounded to 1/1000), everything else as it was when the drag began.
pub fn direct_depth_drag(start: &[f32], dragged: &[usize], delta: f32) -> Vec<f32> {
    let mut depths = start.to_vec();
    for &index in dragged {
        if index < start.len() {
            depths[index] = clamp_depth(start[index] + delta);
        }
    }
    depths
}

/// Attached-point drag: moving the pointer up by 100 px adds 0.6 (D `-(y - originY) * 0.006`).
pub fn attached_point_amount(origin_y: f32, mouse_y: f32) -> f32 {
    -(mouse_y - origin_y) * 0.006
}

/// Ring Create: the endpoints for a drag from `start` to `current`. A drag shorter than 1 unit becomes
/// a ring of length `2 * radius_x` through `start` along `angle`.
pub fn ring_endpoints(start: [f32; 2], current: [f32; 2], settings: &BrushSettings) -> ([f32; 2], [f32; 2]) {
    let (mut p0, mut p1) = (start, current);
    if ((p1[0] - p0[0]).powi(2) + (p1[1] - p0[1]).powi(2)).sqrt() < 1.0 {
        let a = settings.angle * std::f32::consts::PI / 180.0;
        let d = [a.cos() * settings.radius_x, a.sin() * settings.radius_x];
        p0 = [p0[0] - d[0], p0[1] - d[1]];
        p1 = [p1[0] + d[0], p1[1] + d[1]];
    }
    (p0, p1)
}

/// The line drawn while a ring is being dragged (a short drag shows a one-sided preview).
pub fn ring_preview_line(start: [f32; 2], current: [f32; 2], settings: &BrushSettings) -> ([f32; 2], [f32; 2]) {
    let mut p1 = current;
    if ((p1[0] - start[0]).powi(2) + (p1[1] - start[1]).powi(2)).sqrt() < 1.0 {
        let a = settings.angle * std::f32::consts::PI / 180.0;
        p1 = [start[0] + a.cos() * settings.radius_x, start[1] + a.sin() * settings.radius_x];
    }
    (start, p1)
}

/// Plane tool: the radii while dragging, at least 4 units each.
pub fn plane_radii(center: [f32; 2], point: [f32; 2]) -> (f32, f32) {
    ((point[0] - center[0]).abs().max(4.0), (point[1] - center[1]).abs().max(4.0))
}

fn num(v: f32) -> Value {
    json!(f64::from(v))
}

/// The `depth.ops.add` / `depth.ops.set` entry for an attached point.
pub fn attached_point_op(index: usize, amount: f32) -> Value {
    json!({"type": "attached-point", "index": index, "amount": num(amount)})
}

/// The entry for a ring created with `settings` (width is `radius_y`, as D's ring constructor).
pub fn ring_op(p0: [f32; 2], p1: [f32; 2], settings: &BrushSettings) -> Value {
    json!({
        "type": "ring", "p0": [num(p0[0]), num(p0[1])], "p1": [num(p1[0]), num(p1[1])],
        "amount": num(settings.amount), "width": num(settings.radius_y), "hardness": num(settings.hardness),
        "p0Angle": 180.0, "p1Angle": 0.0,
    })
}

/// The entry for a plane created with `settings` (`amount` is the target depth).
pub fn plane_op(center: [f32; 2], radius_x: f32, radius_y: f32, settings: &BrushSettings) -> Value {
    json!({
        "type": "plane", "center": [num(center[0]), num(center[1])], "radiusX": num(radius_x),
        "radiusY": num(radius_y), "angle": num(settings.angle), "targetDepth": num(settings.amount),
        "flattenStrength": num(settings.flatten_strength),
    })
}

/// Label of an op in the "Depth Deformers" list (D `label()`), and its value text (`valueLabel()`).
pub fn op_label(op: &Value) -> String {
    let pair = |key: &str| {
        let list = op.get(key).and_then(Value::as_array);
        (
            list.and_then(|l| l.first()).and_then(Value::as_f64).unwrap_or(0.0),
            list.and_then(|l| l.get(1)).and_then(Value::as_f64).unwrap_or(0.0),
        )
    };
    match op.get("type").and_then(Value::as_str) {
        Some("attached-point") => format!("attached point {}", op.get("index").and_then(Value::as_u64).unwrap_or(0)),
        Some("ring") => {
            let p0 = pair("p0");
            format!("ring {:.0},{:.0}", p0.0, p0.1)
        }
        Some("plane") => {
            let c = pair("center");
            format!("plane {:.0},{:.0}", c.0, c.1)
        }
        _ => "operation".to_owned(),
    }
}

pub fn op_value_label(op: &Value) -> String {
    let key = if op.get("type").and_then(Value::as_str) == Some("plane") { "targetDepth" } else { "amount" };
    format!("{:.2}", op.get(key).and_then(Value::as_f64).unwrap_or(0.0))
}

/// Which handle of an existing ring a pointer grabs (Ring tool, Edit mode).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingHandle {
    P0,
    P1,
    Body,
}

fn distance_to_segment(point: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let v = [b[0] - a[0], b[1] - a[1]];
    let len2 = v[0] * v[0] + v[1] * v[1];
    if len2 <= 0.000001 {
        return ((point[0] - a[0]).powi(2) + (point[1] - a[1]).powi(2)).sqrt();
    }
    let t = (((point[0] - a[0]) * v[0] + (point[1] - a[1]) * v[1]) / len2).clamp(0.0, 1.0);
    let closest = [a[0] + v[0] * t, a[1] + v[1] * t];
    ((point[0] - closest[0]).powi(2) + (point[1] - closest[1]).powi(2)).sqrt()
}

/// D `DepthRingOperation.hit` for the handles the top view can tell apart: the two endpoints, then the ring
/// body (here the p0-p1 segment). `radius` is in the same units as the points.
pub fn ring_hit(p0: [f32; 2], p1: [f32; 2], mouse: [f32; 2], radius: f32) -> Option<(RingHandle, f32)> {
    let dist = |p: [f32; 2]| ((mouse[0] - p[0]).powi(2) + (mouse[1] - p[1]).powi(2)).sqrt();
    let (d0, d1) = (dist(p0), dist(p1));
    if d0 <= radius && d0 <= d1 {
        return Some((RingHandle::P0, d0));
    }
    if d1 <= radius {
        return Some((RingHandle::P1, d1));
    }
    if d0 <= radius {
        return Some((RingHandle::P0, d0));
    }
    let body = distance_to_segment(mouse, p0, p1);
    (body <= radius).then_some((RingHandle::Body, body))
}

/// D `DepthRingOperation.drag` for the three handles: P0/P1 follow the pointer (snapped to a vertex when
/// asked), Body moves both by the pointer travel since the drag began.
pub fn ring_drag(
    handle: RingHandle,
    start_p0: [f32; 2],
    start_p1: [f32; 2],
    start_local: [f32; 2],
    current_local: [f32; 2],
    snap_points: Option<&[[f32; 2]]>,
) -> ([f32; 2], [f32; 2]) {
    let snap = |p: [f32; 2]| snap_points.map_or(p, |points| snap_to_vertex(points, p));
    match handle {
        RingHandle::P0 => (snap(current_local), start_p1),
        RingHandle::P1 => (start_p0, snap(current_local)),
        RingHandle::Body => {
            let d = [current_local[0] - start_local[0], current_local[1] - start_local[1]];
            (snap([start_p0[0] + d[0], start_p0[1] + d[1]]), snap([start_p1[0] + d[0], start_p1[1] + d[1]]))
        }
    }
}

/// How D `DepthMeshEditor.applyToTargets` commits one target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplyPlan {
    /// Neither the operation list nor the depths changed.
    Nothing,
    /// Direct edits bake the preview: drop the saved operations (if any), then write the depths.
    ClearOpsThenSetDepths,
    /// Direct edits on a target without saved operations: write the depths.
    SetDepths,
    /// The operation list changed: replace it, then bake it into the depths.
    SetOpsThenApply,
}

pub fn apply_plan(has_stored_ops: bool, ops_changed: bool, direct_dirty: bool) -> ApplyPlan {
    if direct_dirty {
        if has_stored_ops {
            ApplyPlan::ClearOpsThenSetDepths
        } else {
            ApplyPlan::SetDepths
        }
    } else if ops_changed {
        ApplyPlan::SetOpsThenApply
    } else {
        ApplyPlan::Nothing
    }
}

/// The overlay colour of a depth between `min` and `max`: blue for the back, light grey at 0, orange for the
/// front (the same hues D uses for negative and positive operations). A flat range is grey.
pub fn depth_ramp_color(depth: f32, min: f32, max: f32) -> [u8; 4] {
    const BACK: [f32; 3] = [0.18, 0.40, 0.95];
    const MID: [f32; 3] = [0.85, 0.85, 0.85];
    const FRONT: [f32; 3] = [1.0, 0.48, 0.12];
    let span = max - min;
    if !depth.is_finite() || !span.is_finite() || span <= 1e-6 {
        return [217, 217, 217, 255];
    }
    let t = ((depth - min) / span).clamp(0.0, 1.0);
    let (from, to, f) = if t < 0.5 { (BACK, MID, t * 2.0) } else { (MID, FRONT, (t - 0.5) * 2.0) };
    let channel = |i: usize| ((from[i] + (to[i] - from[i]) * f) * 255.0).round() as u8;
    [channel(0), channel(1), channel(2), 255]
}

/// D `depthOperationColor`: positive operations blue, negative orange, flat grey-blue; brighter when selected.
pub fn operation_color(amount: f32, selected: bool) -> [u8; 4] {
    let c = if amount > 0.000001 {
        if selected { [0.0, 0.8, 1.0, 1.0] } else { [0.05, 0.62, 1.0, 0.92] }
    } else if amount < -0.000001 {
        if selected { [1.0, 0.48, 0.22, 1.0] } else { [1.0, 0.32, 0.18, 0.92] }
    } else if selected {
        [0.1, 0.4, 0.9, 1.0]
    } else {
        [0.1, 0.4, 0.9, 0.9]
    };
    [(c[0] * 255.0f32).round() as u8, (c[1] * 255.0f32).round() as u8, (c[2] * 255.0f32).round() as u8, (c[3] * 255.0f32).round() as u8]
}

// ------------------------------------------------------------------------------------ png layers

const PNG_RETAINED_BYTES_PER_PIXEL: u64 = 8;
const MAX_PNG_RETAINED_BYTES: u64 = 256 * 1024 * 1024;

/// D `ngReserveDepthDrawPngRetainedLayer`: false when this layer would push the retained pixels past 256 MiB.
pub fn reserve_png_retained_layer(width: u64, height: u64, retained_bytes: &mut u64) -> bool {
    let Some(pixels) = width.checked_mul(height) else { return false };
    let Some(required) = pixels.checked_mul(PNG_RETAINED_BYTES_PER_PIXEL) else { return false };
    if *retained_bytes > MAX_PNG_RETAINED_BYTES || required > MAX_PNG_RETAINED_BYTES - *retained_bytes {
        return false;
    }
    *retained_bytes += required;
    true
}

/// D `ngLoadDepthDrawPngLayer` over PNG bytes the caller has read: the whole image is one layer whose
/// depth pixels are its RGBA. `path` names the source (its stem is the default id).
pub fn load_png_layer(path: &str, id: &str, bytes: &[u8], retained_bytes: &mut u64) -> Result<Layer, String> {
    let format = image::ImageFormat::Png;
    let (width, height) = image::ImageReader::with_format(std::io::Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|e| format!("cannot read PNG header: {e}"))?;
    if !reserve_png_retained_layer(u64::from(width), u64::from(height), retained_bytes) {
        return Err("PNG layers exceed the DepthDraw retained memory budget".into());
    }
    let decoded = image::load_from_memory_with_format(bytes, format)
        .map_err(|e| format!("cannot decode PNG: {e}"))?
        .to_rgba8();
    if decoded.width() != width || decoded.height() != height {
        return Err("Decoded PNG dimensions do not match the IHDR header".into());
    }
    let stem = std::path::Path::new(path).file_stem().and_then(|s| s.to_str()).unwrap_or("");
    let id = if id.is_empty() { stem.to_owned() } else { id.to_owned() };
    let rgba = decoded.into_raw();
    Ok(Layer {
        layer_path: format!("/{id}"),
        display_name: id.clone(),
        id,
        source_path: path.to_owned(),
        width: width as i32,
        height: height as i32,
        bounds: Rect { left: 0, top: 0, width: width as i32, height: height as i32 },
        depth_pixels: rgba.clone(),
        rgba,
        ..Layer::default()
    })
}
