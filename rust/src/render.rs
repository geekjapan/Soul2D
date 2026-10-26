use crate::bone_deform::{self, Mat4};
use crate::document::TextureBlob;
use image::{DynamicImage, ImageFormat, ImageReader, Limits, RgbaImage};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{self, Cursor, Write};

const MAX_IMAGE_PIXELS: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_TEXTURE_PIXELS: u64 = 16 * 1024 * 1024;
const MAX_RASTER_WORK: u64 = 100 * 1024 * 1024;
/// The refusal of a render that visits more pixels than `MAX_RASTER_WORK`.
pub(crate) const RASTER_WORK_REFUSAL: &str = "render exceeds the 100-million-pixel raster work limit";
const MAX_RENDER_MEMORY_BYTES: u64 = 640 * 1024 * 1024;
/// The refusal of a render whose target-sized layers need more than `MAX_RENDER_MEMORY_BYTES`.
pub(crate) const RENDER_MEMORY_REFUSAL: &str = "render exceeds the 640-megabyte working-memory limit";
pub(crate) const MAX_COMPOSITE_DEPTH: usize = 16;
const MAX_PACKET_SCENE_NODES: usize = 100_000;
const MAX_PACKET_SCENE_DEPTH: usize = 128;
pub(crate) const MAX_PACKET_MASK_REFERENCES: usize = 1_000_000;
const MAX_PACKET_SCOPES: usize = MAX_PACKET_SCENE_NODES + 1;
const NO_TEXTURE: u64 = u32::MAX as u64;

const PACKET_NODE_BYTES: u64 = 256;
const PACKET_DRAW_BYTES: u64 = 1024;
const PACKET_VERTEX_BYTES: u64 = 256;
const PACKET_INDEX_BYTES: u64 = 40;
const PACKET_SCOPE_BYTES: u64 = 64;
const PACKET_SCOPE_ITEM_BYTES: u64 = 32;
const PACKET_MASK_BYTES: u64 = 128;
const PACKET_COMPOSITE_BYTES_PER_PIXEL: u64 = 26;

/// `DynamicComposite` is drawn like `Composite`: its subtree goes into an offscreen layer that is blended with the
/// node's opacity, tint and blend mode. The layer covers the subtree, so a stored mesh is not used.
fn is_composite_type(node_type: &str) -> bool {
    matches!(node_type, "Composite" | "DynamicComposite")
}

#[derive(Clone, Copy)]
struct Draw<'a> {
    node: &'a Value,
    matrix: Mat4,
    z_sort: f64,
    sequence: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SourceKind {
    Part,
    Mask,
    Composite,
}

#[derive(Clone, Copy)]
struct MaskSource<'a> {
    draw: Draw<'a>,
    kind: SourceKind,
    scope: Option<usize>,
}

#[derive(Clone, Copy)]
enum RenderItem<'a> {
    Part(Draw<'a>),
    Composite { draw: Draw<'a>, scope: usize },
}

impl<'a> RenderItem<'a> {
    fn draw(self) -> Draw<'a> {
        match self {
            Self::Part(draw) | Self::Composite { draw, .. } => draw,
        }
    }
}

#[derive(Default)]
struct RenderScope<'a> {
    items: Vec<RenderItem<'a>>,
}

struct Scene<'a> {
    scopes: Vec<RenderScope<'a>>,
    sources: BTreeMap<u64, MaskSource<'a>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum MaskMode {
    Mask,
    DodgeMask,
}

struct MaskBinding {
    source: u64,
    mode: MaskMode,
}

pub(crate) struct CpuTexture {
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) rgba: Vec<u8>,
}

#[derive(Clone, Copy)]
enum BlendMode {
    Cubism53(crate::native_blend::Mode),
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    LinearDodge,
    AddGlow,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
    Subtract,
    LinearBurn,
    LinearLight,
    Hue,
    Color,
    Inverse,
    DestinationIn,
    ClipToLower,
    SliceFromLower,
}

impl BlendMode {
    fn canonical(self) -> &'static str {
        match self {
            Self::Cubism53(_) => "Cubism53",
            Self::Normal => "Normal",
            Self::Multiply => "Multiply",
            Self::Screen => "Screen",
            Self::Overlay => "Overlay",
            Self::Darken => "Darken",
            Self::Lighten => "Lighten",
            Self::ColorDodge => "ColorDodge",
            Self::LinearDodge => "LinearDodge",
            Self::AddGlow => "AddGlow",
            Self::ColorBurn => "ColorBurn",
            Self::HardLight => "HardLight",
            Self::SoftLight => "SoftLight",
            Self::Difference => "Difference",
            Self::Exclusion => "Exclusion",
            Self::Subtract => "Subtract",
            Self::LinearBurn => "LinearBurn",
            Self::LinearLight => "LinearLight",
            Self::Hue => "Hue",
            Self::Color => "Color",
            Self::Inverse => "Inverse",
            Self::DestinationIn => "DestinationIn",
            Self::ClipToLower => "ClipToLower",
            Self::SliceFromLower => "SliceFromLower",
        }
    }

    /// The formula of a mode that `native_blend` blends. The other INX modes keep the blend factors of D, and a
    /// render packet of version 1 carries none of these.
    fn formula(self) -> Option<crate::native_blend::Mode> {
        use crate::native_blend::Mode;
        match self {
            Self::Cubism53(mode) => Some(mode),
            Self::LinearBurn => Some(Mode::LINEAR_BURN),
            Self::LinearLight => Some(Mode::LINEAR_LIGHT),
            Self::Hue => Some(Mode::HUE),
            Self::Color => Some(Mode::COLOR),
            _ => None,
        }
    }
}

struct RenderContext<'a> {
    psd_clip_stacks: bool,
    psd_clipped_nodes: HashSet<u64>,
    deformations: &'a BTreeMap<u32, Vec<[f64; 2]>>,
    welded: &'a HashMap<(u32, usize), [f64; 2]>,
    world_matrices: &'a BTreeMap<u32, Mat4>,
    textures: &'a [TextureBlob],
    decoded_textures: HashMap<usize, CpuTexture>,
    /// The reduced levels of each texture a Part samples with mipmaps, level 1 first.
    mip_chains: HashMap<usize, Vec<CpuTexture>>,
    width: u32,
    height: u32,
    bounds: [f64; 4],
    pixel_count: usize,
    active_memory_bytes: u64,
    raster_work: u64,
    composite_depth: usize,
    active_composite_masks: HashSet<u64>,
    /// Which items of a Solo render this pass draws. A mask source is always drawn whole.
    solo: Option<SoloPass<'a>>,
}

/// The two passes of a Solo render over the solo set: the rest of the picture under the cover, then the set.
#[derive(Clone, Copy)]
enum SoloPass<'a> {
    /// Every item outside the set.
    Rest(&'a HashSet<u64>),
    /// The Parts in the set and the Composites that hold one.
    Only(&'a HashSet<u64>),
}

impl RenderContext<'_> {
    fn ensure_texture(&mut self, node_id: u64, texture_index: usize) -> Result<(), String> {
        if self.decoded_textures.contains_key(&texture_index) {
            return Ok(());
        }
        let blob = self.textures.get(texture_index).ok_or_else(|| {
            format!("Part node {node_id} refers to missing texture slot {texture_index}")
        })?;
        let decoded = decode_texture(blob, texture_index)?;
        self.reserve_memory(decoded.rgba.len() as u64)?;
        self.decoded_textures.insert(texture_index, decoded);
        Ok(())
    }

    fn ensure_mips(&mut self, node_id: u64, texture_index: usize) -> Result<(), String> {
        self.ensure_texture(node_id, texture_index)?;
        if self.mip_chains.contains_key(&texture_index) {
            return Ok(());
        }
        let chain = mip_chain(&self.decoded_textures[&texture_index]);
        self.reserve_memory(chain.iter().map(|level| level.rgba.len() as u64).sum())?;
        self.mip_chains.insert(texture_index, chain);
        Ok(())
    }

    fn reserve_memory(&mut self, bytes: u64) -> Result<(), String> {
        let next = self
            .active_memory_bytes
            .checked_add(bytes)
            .ok_or_else(|| "render memory limit overflow".to_string())?;
        if next > MAX_RENDER_MEMORY_BYTES {
            return Err(RENDER_MEMORY_REFUSAL.to_string());
        }
        self.active_memory_bytes = next;
        Ok(())
    }

    fn release_memory(&mut self, bytes: u64) {
        self.active_memory_bytes -= bytes;
    }

    fn charge_work(&mut self, work: u64) -> Result<(), String> {
        charge_raster_work(&mut self.raster_work, work)
    }

    fn enter_composite(&mut self) -> Result<(), String> {
        if self.composite_depth >= MAX_COMPOSITE_DEPTH {
            return Err("render exceeds the 16-level Composite nesting limit".to_string());
        }
        self.composite_depth += 1;
        Ok(())
    }

    fn leave_composite(&mut self) {
        self.composite_depth -= 1;
    }

    fn with_stencil<T>(
        &mut self,
        operation: impl FnOnce(&mut [u8], &mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let bytes = self.pixel_count as u64;
        self.reserve_memory(bytes)?;
        let mut stencil = vec![0; self.pixel_count];
        let result = operation(&mut stencil, self);
        self.release_memory(bytes);
        result
    }

    fn with_scratch_layer<T>(
        &mut self,
        operation: impl FnOnce(&mut [f32], &mut [u8], &mut Self) -> Result<T, String>,
    ) -> Result<T, String> {
        let bytes = self.pixel_count as u64 * (4 * std::mem::size_of::<f32>() as u64 + 1);
        // The layer is target-sized whatever is drawn into it, so zeroing it is one pass of real work. It also
        // stands for the caller's pass over the written box, which is never larger.
        self.charge_work(self.pixel_count as u64)?;
        self.reserve_memory(bytes)?;
        let mut canvas = vec![0.0; self.pixel_count * 4];
        let mut coverage = vec![0; self.pixel_count];
        let result = operation(&mut canvas, &mut coverage, self);
        self.release_memory(bytes);
        result
    }
}

fn charge_raster_work(raster_work: &mut u64, work: u64) -> Result<(), String> {
    *raster_work = raster_work
        .checked_add(work)
        .ok_or_else(|| "render work limit overflow".to_string())?;
    if *raster_work > MAX_RASTER_WORK {
        return Err(RASTER_WORK_REFUSAL.to_string());
    }
    Ok(())
}

/// Inclusive pixel box `[x0, y0, x1, y1]` a raster pass may have written to; `None` when it wrote nothing. A
/// composite layer is zero outside the box of its scope, so its passes visit the box instead of the whole target.
type PixelBox = Option<[u32; 4]>;

fn union_box(a: PixelBox, b: PixelBox) -> PixelBox {
    match (a, b) {
        (Some(a), Some(b)) => Some([a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]),
        (a, b) => a.or(b),
    }
}

fn box_area(bounds: PixelBox) -> u64 {
    bounds.map_or(0, |[x0, y0, x1, y1]| u64::from(x1 - x0 + 1) * u64::from(y1 - y0 + 1))
}

/// Indices of the pixels of `bounds` in a buffer `width` pixels wide.
fn box_pixels(bounds: PixelBox, width: u32) -> impl Iterator<Item = usize> {
    bounds.into_iter().flat_map(move |[x0, y0, x1, y1]| {
        (y0..=y1).flat_map(move |y| (x0..=x1).map(move |x| y as usize * width as usize + x as usize))
    })
}

/// The world positions that replace a Part's projected vertices: those glue (weld) blends, and then every vertex of a
/// Part with form edit offsets in model coordinates (`world`), which apply after glue.
fn weld_world_points(
    draws: &[Draw],
    deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
    world: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<HashMap<(u32, usize), [f64; 2]>, String> {
    if world.is_empty()
        && draws
            .iter()
            .all(|draw| draw.node.get("weldedLinks").is_none())
    {
        return Ok(HashMap::new());
    }
    struct Prepared {
        id: u32,
        matrix: Mat4,
        origin: [f64; 2],
        vertices: Vec<[f64; 2]>,
        links: Vec<(u32, Vec<i64>, f64)>,
    }
    let mut prepared = Vec::new();
    for draw in draws {
        let id = draw
            .node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(|id| u32::try_from(id).ok())
            .ok_or("node.uuid must be an unsigned integer")?;
        let mut arrays =
            load_part_arrays(draw.node, u64::from(id))?.ok_or("Part mesh must be an object")?;
        if let Some(offsets) = deformations.get(&id) {
            if offsets.len() != arrays.vertices.len() {
                return Err(format!("Part node {id} deform offset length mismatch"));
            }
            for (vertex, offset) in arrays.vertices.iter_mut().zip(offsets) {
                vertex[0] += offset[0];
                vertex[1] += offset[1];
            }
        }
        let mut links = Vec::new();
        if let Some(raw) = draw.node.get("weldedLinks") {
            let raw = raw
                .as_array()
                .ok_or_else(|| format!("Part node {id}.weldedLinks must be an array"))?;
            for link in raw {
                let target = link
                    .get("targetUUID")
                    .and_then(Value::as_u64)
                    .and_then(|id| u32::try_from(id).ok())
                    .ok_or_else(|| format!("Part node {id} weld target must be a UUID"))?;
                let weight = link
                    .get("weight")
                    .and_then(Value::as_f64)
                    .filter(|weight| weight.is_finite())
                    .ok_or_else(|| format!("Part node {id} weld weight must be finite"))?;
                if !(0.0..=1.0).contains(&weight) {
                    return Err(format!(
                        "Part node {id} weld weight must be between 0 and 1"
                    ));
                }
                let indices = link
                    .get("indices")
                    .and_then(Value::as_array)
                    .ok_or_else(|| format!("Part node {id} weld indices must be an array"))?
                    .iter()
                    .map(|value| {
                        value
                            .as_i64()
                            .ok_or_else(|| format!("Part node {id} weld index must be an integer"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                links.push((target, indices, weight));
            }
        }
        prepared.push(Prepared {
            id,
            matrix: draw.matrix,
            origin: arrays.origin,
            vertices: arrays.vertices,
            links,
        });
    }
    let mut overrides = HashMap::new();
    for part in &prepared {
        for (index, vertex) in part.vertices.iter().enumerate() {
            let mut chosen = None;
            for (target, indices, weight) in &part.links {
                let mapped = indices.get(index).copied().unwrap_or(-1);
                if mapped < 0 {
                    continue;
                }
                if chosen.is_some() {
                    return Err(format!(
                        "Part node {} welds one vertex to two targets",
                        part.id
                    ));
                }
                chosen = Some((*target, indices, *weight, mapped));
            }
            let Some((target, _indices, weight, other_index)) = chosen else {
                continue;
            };
            let other_index = other_index as usize;
            let Some(other) = prepared.iter().find(|item| item.id == target) else {
                continue;
            };
            let reciprocal = other.links.iter().any(|(back, back_indices, _)| {
                *back == part.id
                    && back_indices
                        .get(other_index)
                        .copied()
                        .is_some_and(|index| index >= 0)
            });
            if !reciprocal {
                continue;
            }
            let Some(other_vertex) = other.vertices.get(other_index) else {
                continue;
            };
            let self_world = project(
                part.matrix,
                [vertex[0] - part.origin[0], vertex[1] - part.origin[1]],
            )?;
            let other_world = project(
                other.matrix,
                [
                    other_vertex[0] - other.origin[0],
                    other_vertex[1] - other.origin[1],
                ],
            )?;
            let blended = [
                weight * self_world[0] + (1.0 - weight) * other_world[0],
                weight * self_world[1] + (1.0 - weight) * other_world[1],
            ];
            if blended.iter().any(|value| !value.is_finite()) {
                return Err(format!("Part node {} weld is not finite", part.id));
            }
            overrides.insert((part.id, index), blended);
        }
    }
    for part in &prepared {
        let Some(offsets) = world.get(&part.id).filter(|offsets| offsets.len() == part.vertices.len()) else {
            continue;
        };
        for (index, (vertex, offset)) in part.vertices.iter().zip(offsets).enumerate() {
            let base = match overrides.get(&(part.id, index)) {
                Some(point) => *point,
                None => project(part.matrix, [vertex[0] - part.origin[0], vertex[1] - part.origin[1]])?,
            };
            overrides.insert((part.id, index), [base[0] + offset[0], base[1] + offset[1]]);
        }
    }
    Ok(overrides)
}

#[cfg(test)]
pub(crate) fn scene_bounds(evaluated: &Value) -> Result<[f64; 4], String> {
    scene_bounds_with(evaluated, &BTreeMap::new())
}

#[cfg(test)]
pub(crate) fn scene_bounds_with(
    evaluated: &Value,
    deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<[f64; 4], String> {
    let geometry = crate::scene_deform::evaluate(evaluated, deformations)?;
    scene_bounds_for_scene(evaluated, &geometry)
}

pub(crate) fn scene_bounds_for_scene(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
) -> Result<[f64; 4], String> {
    let root = evaluated.get("nodes").ok_or("model.nodes is missing")?;
    let deformations = &geometry.local_offsets;
    let scene = collect_scene(root, geometry)?;
    let weld_draws = scene
        .sources
        .values()
        .filter(|source| source.kind != SourceKind::Composite)
        .map(|source| source.draw)
        .collect::<Vec<_>>();
    let welded = weld_world_points(&weld_draws, deformations, &geometry.world_offsets)?;
    let mut bounds = [
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    let mut raster_work = 0;
    let mut pending_scopes = vec![0];
    while let Some(scope_index) = pending_scopes.pop() {
        for item in scene.scopes[scope_index].items.iter().copied() {
            match item {
                RenderItem::Part(draw) => {
                    let id = draw
                        .node
                        .get("uuid")
                        .and_then(Value::as_u64)
                        .ok_or("node.uuid must be an unsigned integer")?;
                    let mut arrays =
                        load_part_arrays(draw.node, id)?.ok_or("Part mesh must be an object")?;
                    if let Some(offsets) =
                        u32::try_from(id).ok().and_then(|id| deformations.get(&id))
                    {
                        if offsets.len() != arrays.vertices.len() {
                            return Err("Part deform offset length mismatch".into());
                        }
                        for (vertex, offset) in arrays.vertices.iter_mut().zip(offsets) {
                            vertex[0] += offset[0];
                            vertex[1] += offset[1];
                            if !vertex[0].is_finite() || !vertex[1].is_finite() {
                                return Err(
                                    "scene bounds contain non-finite world coordinates".into()
                                );
                            }
                        }
                    }
                    for (index, vertex) in arrays.vertices.iter().enumerate() {
                        let point = u32::try_from(id)
                            .ok()
                            .and_then(|id| welded.get(&(id, index)).copied());
                        let point = match point {
                            Some(point) => point,
                            None => project(
                                draw.matrix,
                                [vertex[0] - arrays.origin[0], vertex[1] - arrays.origin[1]],
                            )?,
                        };
                        if point.iter().any(|coordinate| !coordinate.is_finite()) {
                            return Err("scene bounds contain non-finite world coordinates".into());
                        }
                        bounds[0] = bounds[0].min(point[0]);
                        bounds[1] = bounds[1].min(point[1]);
                        bounds[2] = bounds[2].max(point[0]);
                        bounds[3] = bounds[3].max(point[1]);
                    }
                }
                RenderItem::Composite { draw, scope } => {
                    let id = draw
                        .node
                        .get("uuid")
                        .and_then(Value::as_u64)
                        .ok_or("Composite node uuid must be an unsigned integer")?;
                    let offsets = composite_offsets(draw.node, id, deformations)?;
                    if !offsets
                        .is_some_and(|offsets| offsets.iter().any(|offset| *offset != [0.0; 2]))
                    {
                        pending_scopes.push(scope);
                        continue;
                    }
                    if let Some(source_bounds) = composite_source_bounds(
                        draw,
                        deformations,
                        &geometry.world_matrices,
                        &mut raster_work,
                    )? {
                        let offsets = offsets.unwrap();
                        for point in composite_output_corners(id, source_bounds, offsets)? {
                            charge_raster_work(&mut raster_work, 1)?;
                            if point.iter().any(|coordinate| !coordinate.is_finite()) {
                                return Err(
                                    "scene bounds contain non-finite world coordinates".into()
                                );
                            }
                            bounds[0] = bounds[0].min(point[0]);
                            bounds[1] = bounds[1].min(point[1]);
                            bounds[2] = bounds[2].max(point[0]);
                            bounds[3] = bounds[3].max(point[1]);
                        }
                    }
                }
            }
        }
    }
    if bounds[0] == f64::INFINITY {
        return Ok([-1.0, -1.0, 1.0, 1.0]);
    }
    for axis in 0..2 {
        let span = bounds[axis + 2] - bounds[axis];
        let margin = if span > 0.0 { span * 0.1 } else { 1.0 };
        bounds[axis] -= margin;
        bounds[axis + 2] += margin;
    }
    if bounds.iter().any(|coordinate| !coordinate.is_finite()) {
        return Err("scene bounds exceed the finite coordinate range".into());
    }
    Ok(bounds)
}

#[cfg(test)]
pub(crate) fn render_png(
    evaluated: &Value,
    textures: &[TextureBlob],
    width: u32,
    height: u32,
    bounds: [f64; 4],
) -> Result<Vec<u8>, String> {
    render_png_with(evaluated, &BTreeMap::new(), textures, width, height, bounds)
}

#[cfg(test)]
pub(crate) fn render_png_with(
    evaluated: &Value,
    deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
    textures: &[TextureBlob],
    width: u32,
    height: u32,
    bounds: [f64; 4],
) -> Result<Vec<u8>, String> {
    let geometry = crate::scene_deform::evaluate(evaluated, deformations)?;
    render_png_for_scene(evaluated, &geometry, textures, width, height, bounds)
}

#[cfg(test)]
pub(crate) fn render_png_for_scene(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
    width: u32,
    height: u32,
    bounds: [f64; 4],
) -> Result<Vec<u8>, String> {
    render_png_lit(evaluated, geometry, textures, width, height, bounds, [1.0; 3])
}

/// Like `render_png_for_scene`, then applies the engine's final-pass ambient light
/// (lighting.frag: `albedo * vec4(ambientLight, 1)`; no emissive buffer exists on the CPU path).
pub(crate) fn render_png_lit(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
    width: u32,
    height: u32,
    bounds: [f64; 4],
    ambient: [f32; 3],
) -> Result<Vec<u8>, String> {
    render_png_composited(evaluated, geometry, textures, width, height, bounds, ambient, false, None)
}

/// The Solo view of [`render_png_lit`]: only the Parts in `only` and the Composites that hold one. With `cover`
/// (unmultiplied RGBA) they are drawn over the rest of the picture covered by that colour, so a blend mode that
/// reads what is below (Multiply, ClipToLower) has something to read. Each Part is drawn once; a Composite that
/// holds solo Parts and others is drawn in both passes, each time with the children of that pass. `only` must hold
/// every node below a node in it (as `solo_nodes` in lib.rs gives), or a Composite in it draws no child.
#[allow(clippy::too_many_arguments)]
pub(crate) fn render_png_solo(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
    width: u32,
    height: u32,
    bounds: [f64; 4],
    ambient: [f32; 3],
    only: &HashSet<u64>,
    cover: Option<[u8; 4]>,
) -> Result<Vec<u8>, String> {
    let solo = Some((only, cover));
    render_png_composited(evaluated, geometry, textures, width, height, bounds, ambient, false, solo)
}

/// PSD clipping stacks retain their base alpha; ordinary model masks keep their binary stencil behavior.
pub(crate) fn render_png_psd(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
    width: u32,
    height: u32,
    bounds: [f64; 4],
) -> Result<Vec<u8>, String> {
    render_png_composited(evaluated, geometry, textures, width, height, bounds, [1.0; 3], true, None)
}

#[allow(clippy::too_many_arguments)]
fn render_png_composited(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
    width: u32,
    height: u32,
    bounds: [f64; 4],
    ambient: [f32; 3],
    psd_clip_stacks: bool,
    solo: Option<(&HashSet<u64>, Option<[u8; 4]>)>,
) -> Result<Vec<u8>, String> {
    fn clipped_nodes(node: &Value, inherited: bool, out: &mut HashSet<u64>) -> Result<(), String> {
        let id = node["uuid"].as_u64().unwrap_or(0);
        let clipped = inherited || !part_mask_bindings(node, id)?.is_empty();
        let composite = is_composite_type(node["type"].as_str().unwrap_or(""));
        if clipped && (composite || node["type"] == "Part") {
            out.insert(id);
        }
        for child in node["children"].as_array().into_iter().flatten() {
            clipped_nodes(child, clipped && !composite, out)?;
        }
        Ok(())
    }
    if width == 0 || height == 0 {
        return Err("render dimensions must be positive".to_string());
    }
    let pixel_count = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| "render dimensions overflow".to_string())?;
    if pixel_count > MAX_IMAGE_PIXELS {
        return Err("render image exceeds the 16-megapixel limit".to_string());
    }
    if bounds.iter().any(|value| !value.is_finite())
        || bounds[2] <= bounds[0]
        || bounds[3] <= bounds[1]
    {
        return Err("render bounds must be finite and have positive width and height".to_string());
    }
    let root = evaluated
        .get("nodes")
        .ok_or_else(|| "model.nodes is missing".to_string())?;
    let deformations = &geometry.local_offsets;
    let scene = collect_scene(root, geometry)?;
    let pixel_count = usize::try_from(pixel_count).unwrap();
    let mut canvas = vec![0.0f32; pixel_count * 4];
    let source_draws = scene
        .sources
        .values()
        .filter(|source| source.kind != SourceKind::Composite)
        .map(|source| source.draw)
        .collect::<Vec<_>>();
    let welded = weld_world_points(&source_draws, deformations, &geometry.world_offsets)?;
    let mut psd_clipped_nodes = HashSet::new();
    if psd_clip_stacks {
        clipped_nodes(root, false, &mut psd_clipped_nodes)?;
    }
    let mut context = RenderContext {
        psd_clip_stacks,
        psd_clipped_nodes,
        deformations,
        welded: &welded,
        world_matrices: &geometry.world_matrices,
        textures,
        decoded_textures: HashMap::new(),
        mip_chains: HashMap::new(),
        width,
        height,
        bounds,
        pixel_count,
        active_memory_bytes: pixel_count as u64 * 4 * std::mem::size_of::<f32>() as u64,
        raster_work: 0,
        composite_depth: 0,
        active_composite_masks: HashSet::new(),
        solo: None,
    };
    if let Some((only, cover)) = solo {
        if let Some(cover) = cover {
            context.solo = Some(SoloPass::Rest(only));
            render_scope(&scene, 0, &mut canvas, None, &mut context)?;
            // Source-over in premultiplied colour. ponytail: the ambient light below tints the cover too.
            let [r, g, b, a] = cover.map(|channel| f32::from(channel) / 255.0);
            for pixel in canvas.chunks_exact_mut(4) {
                for (channel, value) in pixel.iter_mut().zip([r * a, g * a, b * a, a]) {
                    *channel = value + *channel * (1.0 - a);
                }
            }
        }
        context.solo = Some(SoloPass::Only(only));
    }
    render_scope(&scene, 0, &mut canvas, None, &mut context)?;
    // The canvas is premultiplied, so scaling rgb scales the straight colour and leaves alpha alone.
    for pixel in canvas.chunks_exact_mut(4) {
        for (channel, light) in pixel.iter_mut().zip(ambient) {
            *channel *= light;
        }
    }

    let mut bytes = Vec::with_capacity(canvas.len());
    for pixel in canvas.chunks_exact(4) {
        let alpha = pixel[3].clamp(0.0, 1.0);
        let unpremultiply = |component: f32| {
            if alpha == 0.0 {
                0
            } else {
                ((component / alpha).clamp(0.0, 1.0) * 255.0).round() as u8
            }
        };
        bytes.extend_from_slice(&[
            unpremultiply(pixel[0]),
            unpremultiply(pixel[1]),
            unpremultiply(pixel[2]),
            (alpha * 255.0).round() as u8,
        ]);
    }
    let image = RgbaImage::from_raw(width, height, bytes)
        .ok_or_else(|| "could not create output image".to_string())?;
    let mut output = Cursor::new(Vec::new());
    DynamicImage::ImageRgba8(image)
        .write_to(&mut output, ImageFormat::Png)
        .map_err(|error| format!("could not encode rendered PNG: {error}"))?;
    Ok(output.into_inner())
}

pub(crate) fn render_packet_for_scene(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
) -> Result<Value, String> {
    build_packet(evaluated, geometry, textures, true).map(|(packet, _)| packet)
}

/// `scene.geometry`: the drawn Parts of the render packet in painter order, without any texture pixels.
/// A Composite draws its own scope at its place in the parent scope. `zsort` is the accumulated sort
/// value the painter order came from, and `opacity` the evaluated opacity the Part is drawn with.
pub(crate) fn scene_geometry_for_scene(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
) -> Result<Value, String> {
    let (mut packet, zsorts) = build_packet(evaluated, geometry, textures, false)?;
    let order = painter_order(&packet)?
        .iter()
        .map(|draw| draw["id"].as_u64().and_then(|id| u32::try_from(id).ok()).ok_or("draw id must be a u32"))
        .collect::<Result<Vec<_>, _>>()?;
    // The vertex arrays move out of the packet; `json!` would serialize them into a copy.
    let mut draws: HashMap<u32, Value> = packet["draws"]
        .as_array_mut()
        .map(std::mem::take)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|draw| Some((u32::try_from(draw["id"].as_u64()?).ok()?, draw)))
        .collect();
    let mut parts = Vec::with_capacity(order.len());
    for id in order {
        let mut draw = draws.remove(&id).ok_or("render packet draw is missing")?;
        let object = draw.as_object_mut().ok_or("render packet draw is not an object")?;
        let mut part = Map::new();
        part.insert("id".into(), json!(id));
        part.insert("vertices".into(), object.remove("vertices").unwrap_or(Value::Array(Vec::new())));
        part.insert("indices".into(), object.remove("indices").unwrap_or(Value::Array(Vec::new())));
        part.insert("zsort".into(), json!(zsorts.get(&id).copied().unwrap_or(0.0)));
        if let Some(opacity) = object.get("material").and_then(|material| material.get("opacity")) {
            part.insert("opacity".into(), opacity.clone());
        }
        parts.push(Value::Object(part));
    }
    let mut result = Map::new();
    result.insert("parts".into(), Value::Array(parts));
    Ok(Value::Object(result))
}

/// Index pairs of the triangles of one mesh whose insides overlap, which an advanced blend mode can draw twice and
/// flicker (issue #197). Triangles that only share an edge or a corner do not count, nor do degenerate ones.
/// ponytail: O(n²) over bounding-box hits; sweep along x if meshes of many thousand triangles get slow.
pub(crate) fn overlapping_triangles(vertices: &[[f64; 2]], indices: &[u32]) -> Vec<[usize; 2]> {
    let extent = vertices.iter().flatten().fold(0.0f64, |extent, value| extent.max(value.abs()));
    let epsilon = 1e-9 * extent.max(1.0);
    let corner = |index: u32| vertices.get(index as usize).copied();
    let triangles: Vec<(usize, [[f64; 2]; 3])> = indices
        .chunks_exact(3)
        .enumerate()
        .filter_map(|(at, corners)| Some((at, [corner(corners[0])?, corner(corners[1])?, corner(corners[2])?])))
        .filter(|(_, [a, b, c])| ((b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])).abs() > epsilon)
        .collect();
    // Separating axis test: an edge normal of either triangle along which their projections at most touch.
    let separated = |a: &[[f64; 2]; 3], b: &[[f64; 2]; 3]| {
        [a, b].into_iter().flat_map(|t| (0..3).map(move |i| (t[i], t[(i + 1) % 3]))).any(|(from, to)| {
            let (x, y) = (to[0] - from[0], to[1] - from[1]);
            let length = x.hypot(y);
            let project = |t: &[[f64; 2]; 3]| {
                t.iter().map(|p| (p[0] * -y + p[1] * x) / length).fold((f64::MAX, f64::MIN), |(low, high), d| {
                    (low.min(d), high.max(d))
                })
            };
            let ((a_low, a_high), (b_low, b_high)) = (project(a), project(b));
            a_high <= b_low + epsilon || b_high <= a_low + epsilon
        })
    };
    let mut pairs = Vec::new();
    for (i, (first, a)) in triangles.iter().enumerate() {
        for (second, b) in &triangles[i + 1..] {
            if !separated(a, b) {
                pairs.push([*first, *second]);
            }
        }
    }
    pairs
}

/// The Part draws of `packet` in painter order. A Composite draws its own scope at its place in the parent scope.
fn painter_order(packet: &Value) -> Result<Vec<&Value>, String> {
    fn flatten<'a>(
        scope: usize,
        packet: &'a Value,
        draws: &BTreeMap<u64, &'a Value>,
        depth: usize,
        out: &mut Vec<&'a Value>,
    ) -> Result<(), String> {
        if depth > MAX_COMPOSITE_DEPTH + 1 {
            return Err("render packet scopes nest too deeply".into());
        }
        let ids = packet["scopes"][scope].as_array().ok_or("render packet scope is missing")?;
        for id in ids.iter().filter_map(Value::as_u64) {
            let draw = *draws.get(&id).ok_or("render packet scope names an unknown draw")?;
            match draw["kind"].as_str() {
                Some("Composite") => {
                    if let Some(inner) = draw["scope"].as_u64().and_then(|inner| usize::try_from(inner).ok()) {
                        flatten(inner, packet, draws, depth + 1, out)?;
                    }
                }
                Some("Part") => out.push(draw),
                _ => {}
            }
        }
        Ok(())
    }
    let draws: BTreeMap<u64, &Value> = packet["draws"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|draw| Some((draw["id"].as_u64()?, draw)))
        .collect();
    let mut parts = Vec::new();
    flatten(0, packet, &draws, 0, &mut parts)?;
    Ok(parts)
}

/// The Part draws of the render packet in painter order, as the packet holds them (world `vertices`, `uvs`,
/// `indices`, `textures`, `material`, `masks`), without any texture pixels. For a writer of another format.
pub(crate) fn scene_draws_for_scene(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
) -> Result<Vec<Value>, String> {
    let (packet, _) = build_packet(evaluated, geometry, textures, false)?;
    Ok(painter_order(&packet)?.into_iter().cloned().collect())
}

/// The pixel size of the texture in `blob`, as the renderer decodes it.
pub(crate) fn texture_size(blob: &TextureBlob, slot: usize) -> Result<[u32; 2], String> {
    decode_texture(blob, slot).map(|texture| [texture.width, texture.height])
}

/// The packet and the accumulated zsort of every Part, with or without the decoded texture pixels.
fn build_packet(
    evaluated: &Value,
    geometry: &crate::scene_deform::SceneDeform,
    textures: &[TextureBlob],
    with_pixels: bool,
) -> Result<(Value, BTreeMap<u32, f64>), String> {
    let root = evaluated
        .get("nodes")
        .ok_or_else(|| "model.nodes is missing".to_string())?;
    let (node_count, mask_reference_count, _) = packet_scene_limits(root)?;
    let scene = collect_scene(root, geometry)?;
    if scene.scopes.len() > MAX_PACKET_SCOPES {
        return Err("render packet exceeds the scope limit".to_string());
    }
    if scene.sources.len() > node_count {
        return Err("render packet source count exceeds the node limit".to_string());
    }

    let mut resource_bytes = 0;
    charge_packet_bytes(
        &mut resource_bytes,
        u64::try_from(node_count)
            .ok()
            .and_then(|count| count.checked_mul(PACKET_NODE_BYTES))
            .ok_or_else(|| "render packet resource limit overflow".to_string())?,
    )?;
    charge_packet_bytes(
        &mut resource_bytes,
        u64::try_from(mask_reference_count)
            .ok()
            .and_then(|count| count.checked_mul(PACKET_MASK_BYTES))
            .ok_or_else(|| "render packet resource limit overflow".to_string())?,
    )?;
    charge_packet_bytes(
        &mut resource_bytes,
        u64::try_from(scene.scopes.len())
            .ok()
            .and_then(|count| count.checked_mul(PACKET_SCOPE_BYTES))
            .ok_or_else(|| "render packet resource limit overflow".to_string())?,
    )?;

    let mut scope_values = Vec::with_capacity(scene.scopes.len());
    for scope in &scene.scopes {
        charge_packet_bytes(
            &mut resource_bytes,
            u64::try_from(scope.items.len())
                .ok()
                .and_then(|count| count.checked_mul(PACKET_SCOPE_ITEM_BYTES))
                .ok_or_else(|| "render packet resource limit overflow".to_string())?,
        )?;
        let ids = scope
            .items
            .iter()
            .map(|item| {
                item.draw()
                    .node
                    .get("uuid")
                    .and_then(Value::as_u64)
                    .and_then(|id| u32::try_from(id).ok())
                    .ok_or_else(|| "render packet node UUID must be a u32".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        scope_values.push(json!(ids));
    }

    let deformations = &geometry.local_offsets;
    let source_draws = scene
        .sources
        .values()
        .filter(|source| source.kind != SourceKind::Composite)
        .map(|source| source.draw)
        .collect::<Vec<_>>();
    let welded = weld_world_points(&source_draws, deformations, &geometry.world_offsets)?;
    let mut raster_work = 0;
    let mut referenced_textures = BTreeMap::<usize, (u32, bool)>::new();
    let mut draw_values = Vec::with_capacity(scene.sources.len());

    for source in scene.sources.values() {
        let draw = source.draw;
        let id = draw
            .node
            .get("uuid")
            .and_then(Value::as_u64)
            .ok_or("node.uuid must be an unsigned integer")?;
        let packet_id = u32::try_from(id)
            .map_err(|_| format!("node {id} UUID does not fit the render packet"))?;
        charge_packet_bytes(&mut resource_bytes, PACKET_DRAW_BYTES)?;

        let (kind, vertices, uvs, indices, texture_slots, composite_data) = match source.kind {
            SourceKind::Part | SourceKind::Mask => {
                let (vertex_count, uv_count, index_count) = packet_mesh_counts(draw.node);
                let geometry_bytes = u64::try_from(vertex_count)
                    .ok()
                    .and_then(|count| count.checked_mul(PACKET_VERTEX_BYTES))
                    .and_then(|bytes| {
                        u64::try_from(uv_count)
                            .ok()
                            .and_then(|count| count.checked_mul(PACKET_VERTEX_BYTES))
                            .and_then(|uv_bytes| bytes.checked_add(uv_bytes))
                    })
                    .and_then(|bytes| {
                        u64::try_from(index_count)
                            .ok()
                            .and_then(|count| count.checked_mul(PACKET_INDEX_BYTES))
                            .and_then(|index_bytes| bytes.checked_add(index_bytes))
                    })
                    .ok_or_else(|| "render packet resource limit overflow".to_string())?;
                charge_packet_bytes(&mut resource_bytes, geometry_bytes)?;

                let mut arrays = load_part_arrays(draw.node, id)?
                    .ok_or_else(|| format!("Part node {id}.mesh must be an object"))?;
                apply_deformations(&mut arrays.vertices, id, deformations)?;
                if arrays.indices.len() % 3 != 0 {
                    return Err(format!(
                        "Part node {id}.mesh.indices must contain triangles"
                    ));
                }
                if arrays.indices.len() as u64 / 3 > 5_000_000 {
                    return Err(format!("Part node {id} exceeds the triangle limit"));
                }
                for index in &arrays.indices {
                    if *index >= arrays.vertices.len() {
                        return Err(format!("Part node {id} has an out-of-range mesh index"));
                    }
                    if *index > u16::MAX as usize {
                        return Err(format!(
                            "Part node {id} mesh index {index} exceeds the u16 limit"
                        ));
                    }
                }

                let texture_slots = if source.kind == SourceKind::Part {
                    packet_texture_slots(draw.node, id, !arrays.indices.is_empty())?
                } else {
                    [None; 3]
                };
                for slot in texture_slots.iter().flatten().copied() {
                    if textures.get(slot).is_none() {
                        return Err(format!(
                            "Part node {id} refers to missing texture slot {slot}"
                        ));
                    }
                }

                let mut part_uvs = arrays.uvs.take().unwrap_or_default();
                if texture_slots[0].is_some() {
                    if part_uvs.len() != arrays.vertices.len() {
                        return Err(format!(
                            "Part node {id} has mismatched vertex and UV counts"
                        ));
                    }
                    if part_uvs.iter().flatten().any(|value| !value.is_finite()) {
                        return Err(format!(
                            "Part node {id} contains invalid transformed vertex or UV data"
                        ));
                    }
                }
                let world = world_vertices(
                    id,
                    &arrays.vertices,
                    arrays.origin,
                    draw.matrix,
                    &welded,
                )?;
                let indices = arrays
                    .indices
                    .into_iter()
                    .map(|index| json!(index as u16))
                    .collect::<Vec<_>>();
                let vertices = world
                    .into_iter()
                    .map(|point| json!(point))
                    .collect::<Vec<_>>();
                let uvs = part_uvs
                    .drain(..)
                    .map(|uv| json!(uv))
                    .collect::<Vec<_>>();
                (
                    if source.kind == SourceKind::Part {
                        "Part"
                    } else {
                        "Mask"
                    },
                    vertices,
                    uvs,
                    indices,
                    texture_slots,
                    None,
                )
            }
            SourceKind::Composite => {
                let offsets = composite_offsets(draw.node, id, deformations)?.unwrap_or([[0.0; 2]; 4]);
                let source_bounds = composite_source_bounds(
                    draw,
                    deformations,
                    &geometry.world_matrices,
                    &mut raster_work,
                )?;
                // The surface belongs to whoever draws the packet. A vertex reader allocates none, so its size and
                // its share of the packet budget do not limit that reader.
                let surface = match source_bounds {
                    Some(bounds) if with_pixels => Some(composite_source_surface(id, bounds)?),
                    _ => None,
                };
                let (vertices, uvs, indices) = if let Some(bounds) = source_bounds {
                    if let Some((width, height, _)) = surface {
                        let pixels = u64::from(width)
                            .checked_mul(u64::from(height))
                            .ok_or_else(|| {
                                format!("Composite node {id} source dimensions overflow")
                            })?;
                        let bytes = pixels
                            .checked_mul(PACKET_COMPOSITE_BYTES_PER_PIXEL)
                            .ok_or_else(|| {
                                "render packet resource limit overflow".to_string()
                            })?;
                        charge_packet_bytes(&mut resource_bytes, bytes)?;
                    }
                    let corners = composite_output_corners(id, bounds, offsets)?;
                    charge_packet_bytes(&mut resource_bytes, 4 * PACKET_VERTEX_BYTES + 6 * PACKET_INDEX_BYTES)?;
                    (
                        corners
                            .into_iter()
                            .map(|point| json!(point))
                            .collect::<Vec<_>>(),
                        vec![
                            json!([0.0, 0.0]),
                            json!([0.0, 1.0]),
                            json!([1.0, 0.0]),
                            json!([1.0, 1.0]),
                        ],
                        vec![json!(0), json!(1), json!(2), json!(2), json!(1), json!(3)],
                    )
                } else {
                    (Vec::new(), Vec::new(), Vec::new())
                };
                (
                    "Composite",
                    vertices,
                    uvs,
                    indices,
                    [None; 3],
                    Some((source.scope.ok_or_else(|| {
                        format!("Composite node {id} has no render scope")
                    })?, source_bounds, surface)),
                )
            }
        };

        let packet_material = packet_material(draw.node, id, kind, with_pixels)?;
        let native_tint = packet_material
            .get("nativeTint")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        for slot in texture_slots.iter().flatten().copied() {
            referenced_textures
                .entry(slot)
                .and_modify(|(_, premultiplied)| *premultiplied |= native_tint)
                .or_insert((packet_id, native_tint));
        }
        let masks = resolve_mask_bindings(draw.node, id, &scene.sources)?
            .into_iter()
            .map(|(_, binding)| {
                let source = u32::try_from(binding.source).map_err(|_| {
                    format!("node {} mask source UUID does not fit the render packet", id)
                })?;
                let mode = match binding.mode {
                    MaskMode::Mask => "Mask",
                    MaskMode::DodgeMask => "DodgeMask",
                };
                Ok(json!({"source": source, "mode": mode}))
            })
            .collect::<Result<Vec<_>, String>>()?;
        let packet_textures = texture_slots
            .map(|slot| slot.map_or(Value::Null, |slot| json!(slot)));
        let mut draw_value = json!({
            "id": packet_id,
            "kind": kind,
            "textures": packet_textures,
            "material": packet_material,
            "masks": masks,
        });
        {
            // The arrays move in; `json!` would serialize them into a copy.
            let object = draw_value
                .as_object_mut()
                .expect("render packet draw is an object");
            object.insert("vertices".into(), Value::Array(vertices));
            object.insert("uvs".into(), Value::Array(uvs));
            object.insert("indices".into(), Value::Array(indices));
        }
        if let Some((scope, source_bounds, surface)) = composite_data {
            let object = draw_value
                .as_object_mut()
                .expect("render packet draw is an object");
            object.insert("scope".into(), json!(scope));
            object.insert(
                "sourceBounds".into(),
                source_bounds.map_or(Value::Null, |bounds| json!(bounds)),
            );
            object.insert(
                "surface".into(),
                surface.map_or(Value::Null, |(width, height, bounds)| {
                    json!({"width": width, "height": height, "bounds": bounds})
                }),
            );
        }
        draw_values.push(draw_value);
    }

    let mut texture_values = Vec::with_capacity(referenced_textures.len());
    let pixel_textures = if with_pixels { referenced_textures } else { BTreeMap::new() };
    for (slot, (owner, premultiplied)) in pixel_textures {
        let blob = textures.get(slot).ok_or_else(|| {
            format!("Part node {owner} refers to missing texture slot {slot}")
        })?;
        let decoded = decode_texture(blob, slot)?;
        let encoded_size = u64::try_from(decoded.rgba.len())
            .ok()
            .and_then(|size| size.checked_add(2))
            .and_then(|size| size.checked_div(3))
            .and_then(|size| size.checked_mul(4))
            .ok_or_else(|| "render packet texture size overflow".to_string())?;
        let texture_bytes = u64::try_from(decoded.rgba.len())
            .ok()
            .and_then(|size| size.checked_add(encoded_size))
            .and_then(|size| size.checked_add(128))
            .ok_or_else(|| "render packet resource limit overflow".to_string())?;
        charge_packet_bytes(&mut resource_bytes, texture_bytes)?;
        let rgba = base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            &decoded.rgba,
        );
        let mut texture = json!({
            "slot": slot,
            "width": decoded.width,
            "height": decoded.height,
            "rgba": rgba,
        });
        if premultiplied {
            texture["premultiplied"] = json!(true);
        }
        texture_values.push(texture);
    }

    let mut packet = json!({
        "version": 1,
        "rootScope": 0,
    });
    {
        let object = packet.as_object_mut().expect("render packet is an object");
        object.insert("scopes".into(), Value::Array(scope_values));
        object.insert("draws".into(), Value::Array(draw_values));
        object.insert("textures".into(), Value::Array(texture_values));
    }
    let mut size_writer = PacketSizeWriter::default();
    if let Err(error) = serde_json::to_writer(&mut size_writer, &packet) {
        if size_writer.exceeded {
            return Err("render packet exceeds the 640-megabyte serialized limit".to_string());
        }
        return Err(format!("could not serialize render packet: {error}"));
    }
    let zsorts = scene
        .sources
        .iter()
        .filter_map(|(id, source)| Some((u32::try_from(*id).ok()?, source.draw.z_sort)))
        .collect();
    Ok((packet, zsorts))
}

#[derive(Default)]
struct PacketSizeWriter {
    bytes: u64,
    exceeded: bool,
}

impl Write for PacketSizeWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let next = self.bytes.checked_add(buffer.len() as u64);
        if next.is_none_or(|bytes| bytes > MAX_RENDER_MEMORY_BYTES) {
            self.exceeded = true;
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "render packet serialized size limit exceeded",
            ));
        }
        self.bytes = next.unwrap();
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Nodes, mask references and the deepest `Composite` nesting of a scene tree; a tree over a render limit is
/// refused.
pub(crate) fn packet_scene_limits(root: &Value) -> Result<(usize, usize, usize), String> {
    let mut pending = vec![(root, 0usize, 0usize)];
    let mut nodes = 0usize;
    let mut mask_references = 0usize;
    let mut deepest_composite = 0usize;
    while let Some((node, depth, composite_depth)) = pending.pop() {
        nodes += 1;
        if nodes > MAX_PACKET_SCENE_NODES {
            return Err("render packet exceeds the 100000-node limit".to_string());
        }
        if depth > MAX_PACKET_SCENE_DEPTH {
            return Err("render packet exceeds the scene depth limit".to_string());
        }
        let composite_depth = composite_depth
            + if is_composite_type(node.get("type").and_then(Value::as_str).unwrap_or("")) {
                1
            } else {
                0
            };
        if composite_depth > MAX_COMPOSITE_DEPTH {
            return Err("scene exceeds the 16-level Composite nesting limit".to_string());
        }
        deepest_composite = deepest_composite.max(composite_depth);
        let bindings = node
            .get("masks")
            .or_else(|| node.get("masked_by"))
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        mask_references = mask_references
            .checked_add(bindings)
            .ok_or_else(|| "render packet mask count overflow".to_string())?;
        if mask_references > MAX_PACKET_MASK_REFERENCES {
            return Err("render packet exceeds the mask reference limit".to_string());
        }
        if let Some(children) = node.get("children") {
            let children = children
                .as_array()
                .ok_or_else(|| "node.children must be an array".to_string())?;
            if nodes
                .checked_add(pending.len())
                .and_then(|count| count.checked_add(children.len()))
                .is_none_or(|count| count > MAX_PACKET_SCENE_NODES)
            {
                return Err("render packet exceeds the 100000-node limit".to_string());
            }
            pending.extend(
                children
                    .iter()
                    .rev()
                    .map(|child| (child, depth + 1, composite_depth)),
            );
        }
    }
    Ok((nodes, mask_references, deepest_composite))
}

fn packet_mesh_counts(node: &Value) -> (usize, usize, usize) {
    let mesh = node.get("mesh").and_then(Value::as_object);
    let pairs = |value: Option<&Value>| {
        value.and_then(Value::as_array).map_or(0, |values| {
            if values.first().is_some_and(Value::is_array) {
                values.len()
            } else {
                values.len() / 2
            }
        })
    };
    if let Some(vertices) = mesh.and_then(|mesh| mesh.get("verts")) {
        return (
            pairs(Some(vertices)),
            pairs(mesh.and_then(|mesh| mesh.get("uvs"))),
            mesh.and_then(|mesh| mesh.get("indices"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        );
    }
    if let Some(vertices) = node.get("vertices") {
        return (
            pairs(Some(vertices)),
            pairs(node.get("uvs")),
            node.get("indices")
                .and_then(Value::as_array)
                .map_or(0, Vec::len),
        );
    }
    (0, 0, 0)
}

fn packet_texture_slots(
    node: &Value,
    id: u64,
    has_triangles: bool,
) -> Result<[Option<usize>; 3], String> {
    let albedo = first_albedo_slot(node, id, has_triangles)?;
    let mut slots = [None; 3];
    let Some(value) = node.get("textures") else {
        return Ok(slots);
    };
    let raw_slots = value
        .as_array()
        .ok_or_else(|| format!("Part node {id}.textures must be an array"))?;
    for (position, raw_slot) in raw_slots.iter().enumerate() {
        let Some(slot) = texture_slot(raw_slot, id, position)? else {
            continue;
        };
        if position > 2 {
            return Err(format!(
                "Part node {id} uses unsupported texture slot {position}"
            ));
        }
        slots[position] = Some(slot);
    }
    slots[0] = albedo;
    Ok(slots)
}

fn packet_material(node: &Value, id: u64, kind: &str, drawn: bool) -> Result<Value, String> {
    let material = material(node, id, kind)?;
    // A packet without pixels is read for its vertices only (`scene.geometry`), so its blend is never drawn.
    if let Some(alpha) = node.get("alphaBlend").and_then(Value::as_str).filter(|alpha| drawn && *alpha != "Over") {
        return Err(format!(
            "{kind} node {id} has alphaBlend {alpha}, which the CPU renderer draws; GPU packets do not support it"
        ));
    }
    if drawn && matches!(material.blend, BlendMode::Cubism53(_)) {
        return Err("advanced native materials require the CPU renderer; GPU packets do not support Cubism 5.3 blending".into());
    }
    if drawn && material.blend.formula().is_some() {
        let mode = material.blend.canonical();
        return Err(format!(
            "{kind} node {id} blends as {mode}, which the CPU renderer draws; GPU packets do not support it"
        ));
    }
    let emission = optional_finite(
        node.get("emissionStrength"),
        1.0,
        &format!("{kind} node {id}.emissionStrength"),
    )? as f32;
    if !emission.is_finite() {
        return Err(format!("{kind} node {id}.emissionStrength is not a finite f32"));
    }
    let threshold = optional_finite(
        node.get("mask_threshold"),
        0.5,
        &format!("{kind} node {id}.mask_threshold"),
    )?
    .clamp(0.0, 1.0) as f32;
    let mut packet = json!({
        "blendMode": material.blend.canonical(),
        "tint": material.tint,
        "screenTint": material.screen_tint,
        "opacity": material.opacity,
        "emissionStrength": emission,
        "maskThreshold": threshold,
    });
    if material.native_tint {
        packet["nativeTint"] = json!(true);
    }
    Ok(packet)
}

fn charge_packet_bytes(total: &mut u64, bytes: u64) -> Result<(), String> {
    let next = total
        .checked_add(bytes)
        .ok_or_else(|| "render packet resource limit overflow".to_string())?;
    if next > MAX_RENDER_MEMORY_BYTES {
        return Err("render packet exceeds the 640-megabyte resource limit".to_string());
    }
    *total = next;
    Ok(())
}

struct SceneIndex<'a> {
    nodes: HashMap<usize, &'a Value>,
    parents: HashMap<usize, Option<usize>>,
    drawable_by_uuid: BTreeMap<u64, usize>,
    visible_parts: Vec<usize>,
    visible_composites: Vec<usize>,
}

fn collect_scene<'a>(
    root: &'a Value,
    deformation: &crate::scene_deform::SceneDeform,
) -> Result<Scene<'a>, String> {
    let mut index = SceneIndex {
        nodes: HashMap::new(),
        parents: HashMap::new(),
        drawable_by_uuid: BTreeMap::new(),
        visible_parts: Vec::new(),
        visible_composites: Vec::new(),
    };
    index_scene_nodes(root, None, true, &mut index);

    let mut geometry = HashSet::new();
    let mut mask_pending = Vec::new();
    let mut mask_source_roots = HashSet::new();
    for pointer in index.visible_parts.iter().chain(&index.visible_composites) {
        if geometry.insert(*pointer) {
            mask_pending.push(*pointer);
        }
    }
    let mut mask_processed = HashSet::new();
    while let Some(pointer) = mask_pending.pop() {
        if !mask_processed.insert(pointer) {
            continue;
        }
        let node = index.nodes[&pointer];
        let id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .ok_or_else(|| "node.uuid must be an unsigned integer".to_string())?;
        for binding in part_mask_bindings(node, id)? {
            if let Some(source) = index.drawable_by_uuid.get(&binding.source) {
                if geometry.insert(*source) {
                    mask_pending.push(*source);
                }
                if is_composite_type(index.nodes[source].get("type").and_then(Value::as_str).unwrap_or("")) {
                    mask_source_roots.insert(*source);
                    include_source_subtree(index.nodes[source], &mut geometry, &mut mask_pending);
                }
            }
        }
    }

    let mut pending = geometry.iter().copied().collect::<Vec<_>>();
    let mut processed = HashSet::new();
    while let Some(pointer) = pending.pop() {
        if !processed.insert(pointer) {
            continue;
        }
        let node = index.nodes[&pointer];
        let id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .ok_or_else(|| "node.uuid must be an unsigned integer".to_string())?;
        for target in welded_target_ids(node, id)? {
            if let Some(target_pointer) = index.drawable_by_uuid.get(&target) {
                if geometry.insert(*target_pointer) {
                    pending.push(*target_pointer);
                    if is_composite_type(
                        index.nodes[target_pointer].get("type").and_then(Value::as_str).unwrap_or(""),
                    ) {
                        include_source_subtree(
                            index.nodes[target_pointer],
                            &mut geometry,
                            &mut pending,
                        );
                    }
                }
            }
        }
    }

    let mut required = geometry.clone();
    for pointer in geometry.iter().copied() {
        let mut ancestor = index.parents.get(&pointer).copied().flatten();
        while let Some(parent) = ancestor {
            required.insert(parent);
            ancestor = index.parents.get(&parent).copied().flatten();
        }
    }
    required.insert(root as *const Value as usize);

    let mut scene = Scene {
        scopes: vec![RenderScope::default()],
        sources: BTreeMap::new(),
    };
    let mut sequence = 0;
    collect_draws(
        root,
        0.0,
        true,
        deformation,
        &geometry,
        &required,
        &mask_source_roots,
        0,
        0,
        &mut sequence,
        &mut scene,
    )?;
    for scope in &mut scene.scopes {
        scope.items.sort_by(|a, b| {
            b.draw()
                .z_sort
                .total_cmp(&a.draw().z_sort)
                .then_with(|| a.draw().sequence.cmp(&b.draw().sequence))
        });
    }
    Ok(scene)
}

fn index_scene_nodes<'a>(
    node: &'a Value,
    parent: Option<usize>,
    parent_visible: bool,
    index: &mut SceneIndex<'a>,
) {
    let pointer = node as *const Value as usize;
    index.nodes.insert(pointer, node);
    index.parents.insert(pointer, parent);

    let visible = parent_visible && node.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    if matches!(node_type, "Part" | "Mask") || is_composite_type(node_type) {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64) {
            index.drawable_by_uuid.entry(id).or_insert(pointer);
        }
    }
    if visible && node_type == "Part" {
        index.visible_parts.push(pointer);
    }
    if visible && is_composite_type(node_type) {
        index.visible_composites.push(pointer);
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            index_scene_nodes(child, Some(pointer), visible, index);
        }
    }
}

fn include_source_subtree(node: &Value, geometry: &mut HashSet<usize>, pending: &mut Vec<usize>) {
    if matches!(node.get("type").and_then(Value::as_str), Some("Part" | "Mask"))
        || is_composite_type(node.get("type").and_then(Value::as_str).unwrap_or(""))
    {
        let pointer = node as *const Value as usize;
        if geometry.insert(pointer) {
            pending.push(pointer);
        }
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            include_source_subtree(child, geometry, pending);
        }
    }
}

/// The four corner offsets that warp a composite. A `DynamicComposite` converted from a Part keeps that Part's
/// mesh, so a deformer can give it any number of offsets; only a quad can warp the layer, so it is drawn unwarped.
fn composite_offsets(
    node: &Value,
    id: u64,
    deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<Option<[[f64; 2]; 4]>, String> {
    let Some(offsets) = u32::try_from(id).ok().and_then(|id| deformations.get(&id)) else {
        return Ok(None);
    };
    if offsets.len() != 4 && node.get("type").and_then(Value::as_str) == Some("DynamicComposite") {
        return Ok(None);
    }
    if offsets.len() != 4 {
        return Err(format!(
            "Composite node {id} deform offset length must be 4"
        ));
    }
    if offsets.iter().flatten().any(|value| !value.is_finite()) {
        return Err(format!("Composite node {id} deform offset is not finite"));
    }
    Ok(Some([offsets[0], offsets[1], offsets[2], offsets[3]]))
}

fn welded_target_ids(node: &Value, id: u64) -> Result<Vec<u64>, String> {
    let Some(raw) = node.get("weldedLinks") else {
        return Ok(Vec::new());
    };
    let raw = raw
        .as_array()
        .ok_or_else(|| format!("Part node {id}.weldedLinks must be an array"))?;
    raw.iter()
        .map(|link| {
            link.get("targetUUID")
                .and_then(Value::as_u64)
                .and_then(|id| u32::try_from(id).ok())
                .map(u64::from)
                .ok_or_else(|| format!("Part node {id} weld target must be a UUID"))
        })
        .collect()
}

fn collect_draws<'a>(
    node: &'a Value,
    parent_z: f64,
    parent_enabled: bool,
    deformation: &crate::scene_deform::SceneDeform,
    geometry: &HashSet<usize>,
    required: &HashSet<usize>,
    mask_source_roots: &HashSet<usize>,
    scope_index: usize,
    composite_depth: usize,
    sequence: &mut usize,
    scene: &mut Scene<'a>,
) -> Result<(), String> {
    let pointer = node as *const Value as usize;
    let enabled = node.get("enabled").and_then(Value::as_bool).unwrap_or(true);
    let visible = parent_enabled && enabled;
    let scope_visible = visible || (parent_enabled && mask_source_roots.contains(&pointer));
    if !scope_visible && !required.contains(&pointer) {
        return Ok(());
    }
    let id = node
        .get("uuid")
        .and_then(Value::as_u64)
        .ok_or_else(|| "node.uuid must be an unsigned integer".to_string())?;
    let node_type = node.get("type").and_then(Value::as_str).unwrap_or("Node");
    let matrix = u32::try_from(id)
        .ok()
        .and_then(|id| deformation.world_matrices.get(&id))
        .copied()
        .ok_or_else(|| format!("node {id} has no evaluated world matrix"))?;
    if matrix.iter().flatten().any(|value| !value.is_finite()) {
        return Err(format!("node {id} transform is not finite"));
    }
    let z_sort = if scope_visible {
        let own_z = optional_finite(node.get("zsort"), 0.0, &format!("node {id}.zsort"))?;
        let z_sort = parent_z + own_z;
        if !z_sort.is_finite() {
            return Err(format!("node {id} accumulated zsort is not finite"));
        }
        z_sort
    } else {
        0.0
    };
    match node_type {
        "Node" | "Root" | "DepthRigRoot" | "DepthBone" | "SimplePhysics" | "GridDeformer"
        | "PathDeformer" | "MeshGroup" | "Camera" => {}
        "Part" => {
            let draw = Draw {
                node,
                matrix,
                z_sort,
                sequence: *sequence,
            };
            if visible {
                *sequence += 1;
                scene.scopes[scope_index].items.push(RenderItem::Part(draw));
            }
            if geometry.contains(&pointer) {
                scene.sources.entry(id).or_insert(MaskSource {
                    draw,
                    kind: SourceKind::Part,
                    scope: None,
                });
            }
        }
        "Mask" => {
            if geometry.contains(&pointer) {
                scene.sources.entry(id).or_insert(MaskSource {
                    draw: Draw {
                        node,
                        matrix,
                        z_sort,
                        sequence: *sequence,
                    },
                    kind: SourceKind::Mask,
                    scope: None,
                });
            }
        }
        "Composite" | "DynamicComposite" => {
            if composite_depth >= MAX_COMPOSITE_DEPTH {
                return Err("scene exceeds the 16-level Composite nesting limit".into());
            }
            let _ = composite_offsets(node, id, &deformation.local_offsets)?;
            let child_scope = scene.scopes.len();
            scene.scopes.push(RenderScope::default());
            let draw = Draw {
                node,
                matrix,
                z_sort,
                sequence: *sequence,
            };
            let include_in_parent = visible;
            if include_in_parent {
                *sequence += 1;
                scene.scopes[scope_index].items.push(RenderItem::Composite {
                    draw,
                    scope: child_scope,
                });
            }
            if geometry.contains(&pointer) {
                scene.sources.entry(id).or_insert(MaskSource {
                    draw,
                    kind: SourceKind::Composite,
                    scope: Some(child_scope),
                });
            }
            if let Some(children) = node.get("children") {
                let children = children
                    .as_array()
                    .ok_or_else(|| format!("node {id}.children must be an array"))?;
                for child in children {
                    collect_draws(
                        child,
                        z_sort,
                        scope_visible,
                        deformation,
                        geometry,
                        required,
                        mask_source_roots,
                        child_scope,
                        composite_depth + 1,
                        sequence,
                        scene,
                    )?;
                }
            }
            return Ok(());
        }
        other if visible => {
            return Err(format!("node {id} has unsupported type {other:?}"));
        }
        _ => {}
    }
    if let Some(children) = node.get("children") {
        let children = children
            .as_array()
            .ok_or_else(|| format!("node {id}.children must be an array"))?;
        for child in children {
            collect_draws(
                child,
                z_sort,
                visible,
                deformation,
                geometry,
                required,
                mask_source_roots,
                scope_index,
                composite_depth,
                sequence,
                scene,
            )?;
        }
    }
    Ok(())
}

pub(crate) fn node_matrix(node: &Value, id: u64) -> Result<Mat4, String> {
    let transform = node
        .get("transform")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("node {id}.transform must be an object"))?;
    let translation = vector(
        transform.get("trans"),
        3,
        [0.0, 0.0, 0.0],
        &format!("node {id}.transform.trans"),
    )?;
    let rotation = vector(
        transform.get("rot"),
        3,
        [0.0, 0.0, 0.0],
        &format!("node {id}.transform.rot"),
    )?;
    let scale = vector(
        transform.get("scale"),
        2,
        [1.0, 1.0],
        &format!("node {id}.transform.scale"),
    )?;
    bone_deform::projection_matrix(translation, rotation, [scale[0], scale[1]])
        .map_err(|_| format!("node {id} transform is not finite"))
}

pub(crate) fn pin_to_parent_mesh(
    node: &Value,
    id: u64,
    parent_node: Option<&Value>,
    parent_world: Mat4,
    child_world: Mat4,
    locked: bool,
    deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<PinnedNodeTransform, String> {
    let parent = parent_node.ok_or_else(|| format!("node {id} pinToMesh parent must be a Part"))?;
    let parent_type = parent.get("type").and_then(Value::as_str).unwrap_or("Node");
    if !matches!(parent_type, "Part" | "Mask") {
        return Err(format!("node {id} pinToMesh parent must be a Part"));
    }
    let parent_id = parent
        .get("uuid")
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("node {id} pinToMesh parent must be a Part"))?;
    let Some(surface) = pin_surface(parent, parent_id, deformations)? else {
        return pinned_node_transform(node, id);
    };
    let (translation, _rotation, _scale) = local_trs(node, id)?;
    // An unlocked child's origin in parent space is its local translation.
    // That matches parent.inverse * worldOrigin when the parent is invertible,
    // and stays defined when a finite zero scale makes that matrix singular.
    let point = if locked {
        let world_origin = [child_world[0][3], child_world[1][3], child_world[2][3]];
        let local = bone_deform::transform_point(
            bone_deform::invert(parent_world)
                .map_err(|_| format!("node {parent_id} pinToMesh mesh is not finite"))?,
            world_origin,
        )
        .map_err(|_| format!("node {parent_id} pinToMesh mesh is not finite"))?;
        [local[0], local[1]]
    } else {
        [translation[0], translation[1]]
    };
    if !point[0].is_finite() || !point[1].is_finite() {
        return Err(format!("node {parent_id} pinToMesh mesh is not finite"));
    }
    let Some(triangle) = surrounding_triangle(&surface.rest, &surface.indices, point) else {
        return pinned_node_transform(node, id);
    };
    let mapped = affine_point(&surface.rest, &surface.deformed, triangle, point)
        .map_err(|_| format!("node {parent_id} pinToMesh mesh is not finite"))?;
    let delta = [mapped[0] - point[0], mapped[1] - point[1]];
    let angle = pin_rotation(&surface.rest, &surface.deformed, triangle, point, mapped)
        .map_err(|_| format!("node {parent_id} pinToMesh mesh is not finite"))?;
    if !delta[0].is_finite() || !delta[1].is_finite() || !angle.is_finite() {
        return Err(format!("node {parent_id} pinToMesh mesh is not finite"));
    }
    let (mut translation, mut rotation, scale) = local_trs(node, id)?;
    translation[0] += delta[0];
    translation[1] += delta[1];
    rotation[2] += angle;
    let local_matrix = bone_deform::projection_matrix(translation, rotation, scale)
        .map_err(|_| format!("node {id} pinToMesh mesh is not finite"))?;
    if local_matrix
        .iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(format!("node {id} pinToMesh mesh is not finite"));
    }
    Ok(PinnedNodeTransform {
        local_matrix,
        local_rotation_z: rotation[2],
    })
}

pub(crate) struct PinnedNodeTransform {
    pub(crate) local_matrix: Mat4,
    pub(crate) local_rotation_z: f64,
}

fn pinned_node_transform(node: &Value, id: u64) -> Result<PinnedNodeTransform, String> {
    let (translation, rotation, scale) = local_trs(node, id)?;
    let local_matrix = bone_deform::projection_matrix(translation, rotation, scale)
        .map_err(|_| format!("node {id} pinToMesh mesh is not finite"))?;
    Ok(PinnedNodeTransform {
        local_matrix,
        local_rotation_z: rotation[2],
    })
}

struct PinSurface {
    rest: Vec<[f64; 2]>,
    deformed: Vec<[f64; 2]>,
    indices: Vec<usize>,
}

struct PartArrays {
    vertices: Vec<[f64; 2]>,
    uvs: Option<Vec<[f64; 2]>>,
    indices: Vec<usize>,
    origin: [f64; 2],
}

pub(crate) fn part_vertex_count(node: &Value, id: u32) -> Result<usize, String> {
    Ok(load_part_arrays(node, u64::from(id))?
        .map(|arrays| arrays.vertices.len())
        .unwrap_or(0))
}

fn load_part_arrays(node: &Value, id: u64) -> Result<Option<PartArrays>, String> {
    let mesh = node.get("mesh").and_then(Value::as_object);
    let origin = vector(
        mesh.and_then(|mesh| mesh.get("origin")),
        2,
        [0.0, 0.0],
        &format!("Part node {id}.mesh.origin"),
    )?;
    if mesh.and_then(|mesh| mesh.get("verts")).is_some() {
        let path = format!("Part node {id}.mesh");
        let uvs = if mesh.and_then(|mesh| mesh.get("uvs")).is_some() {
            Some(flat_pairs(
                mesh.and_then(|mesh| mesh.get("uvs")),
                &format!("{path}.uvs"),
            )?)
        } else {
            None
        };
        return Ok(Some(PartArrays {
            vertices: flat_pairs(
                mesh.and_then(|mesh| mesh.get("verts")),
                &format!("{path}.verts"),
            )?,
            uvs,
            indices: mesh_indices(mesh.and_then(|mesh| mesh.get("indices")), &path)?,
            origin,
        }));
    }
    if node.get("vertices").is_some() {
        let path = format!("Part node {id}");
        let uvs = if node.get("uvs").is_some() {
            Some(coordinate_pairs(node.get("uvs"), &format!("{path}.uvs"))?)
        } else {
            None
        };
        return Ok(Some(PartArrays {
            vertices: coordinate_pairs(node.get("vertices"), &format!("{path}.vertices"))?,
            uvs,
            indices: mesh_indices(node.get("indices"), &path)?,
            origin,
        }));
    }
    Ok(None)
}

fn pin_surface(
    parent: &Value,
    parent_id: u64,
    deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<Option<PinSurface>, String> {
    let path = format!("node {parent_id} pinToMesh mesh");
    let Some(arrays) = load_part_arrays(parent, parent_id)? else {
        return Ok(None);
    };
    let vertices = arrays.vertices;
    let indices = arrays.indices;
    let origin = arrays.origin;
    if let Some(offsets) = u32::try_from(parent_id)
        .ok()
        .and_then(|id| deformations.get(&id))
    {
        if offsets
            .iter()
            .any(|offset| !offset[0].is_finite() || !offset[1].is_finite())
        {
            return Err(format!("{path} is not finite"));
        }
    }
    if indices.is_empty() {
        if let Some(offsets) = u32::try_from(parent_id)
            .ok()
            .and_then(|id| deformations.get(&id))
        {
            if offsets.len() != vertices.len() {
                return Err(format!("{path} deform offset length mismatch"));
            }
        }
        return Ok(None);
    }
    if indices.len() % 3 != 0 {
        return Err(format!("{path} indices must contain triangles"));
    }
    for index in &indices {
        if *index >= vertices.len() {
            return Err(format!("{path} index is out of range"));
        }
    }
    let rest = vertices
        .iter()
        .map(|vertex| [vertex[0] - origin[0], vertex[1] - origin[1]])
        .collect::<Vec<_>>();
    let mut deformed = rest.clone();
    if let Some(offsets) = u32::try_from(parent_id)
        .ok()
        .and_then(|id| deformations.get(&id))
    {
        if offsets.len() != deformed.len() {
            return Err(format!("{path} deform offset length mismatch"));
        }
        for (vertex, offset) in deformed.iter_mut().zip(offsets) {
            if !offset[0].is_finite() || !offset[1].is_finite() {
                return Err(format!("{path} is not finite"));
            }
            vertex[0] += offset[0];
            vertex[1] += offset[1];
            if !vertex[0].is_finite() || !vertex[1].is_finite() {
                return Err(format!("{path} is not finite"));
            }
        }
    }
    Ok(Some(PinSurface {
        rest,
        deformed,
        indices,
    }))
}

fn mesh_indices(value: Option<&Value>, path: &str) -> Result<Vec<usize>, String> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path} indices must be an array"))?;
    values
        .iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| format!("{path} index is out of range"))
        })
        .collect()
}

fn coordinate_pairs(value: Option<&Value>, path: &str) -> Result<Vec<[f64; 2]>, String> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path} must be an array"))?;
    if values.first().is_some_and(Value::is_array) {
        return values
            .iter()
            .map(|item| {
                let pair = item
                    .as_array()
                    .filter(|pair| pair.len() >= 2)
                    .ok_or_else(|| format!("{path} must contain coordinate pairs"))?;
                Ok([
                    finite_number(&pair[0], path)?,
                    finite_number(&pair[1], path)?,
                ])
            })
            .collect();
    }
    flat_pairs(value, path)
}

fn surrounding_triangle(
    vertices: &[[f64; 2]],
    indices: &[usize],
    point: [f64; 2],
) -> Option<[usize; 3]> {
    fn sign(point: [f64; 2], a: [f64; 2], b: [f64; 2]) -> f64 {
        (point[0] - b[0]) * (a[1] - b[1]) - (a[0] - b[0]) * (point[1] - b[1])
    }
    for triangle in indices.chunks_exact(3) {
        let corners = [
            vertices[triangle[0]],
            vertices[triangle[1]],
            vertices[triangle[2]],
        ];
        let area = (corners[1][0] - corners[0][0]) * (corners[2][1] - corners[0][1])
            - (corners[2][0] - corners[0][0]) * (corners[1][1] - corners[0][1]);
        // A zero-area triangle reports every point as inside. Skip only a
        // triangle that invert_mat3 would reject, relative to its edge length,
        // so a small but well-shaped triangle can still pin the child.
        if singular_triangle(area, corners) {
            continue;
        }
        let signs = [
            sign(point, corners[0], corners[1]),
            sign(point, corners[1], corners[2]),
            sign(point, corners[2], corners[0]),
        ];
        let negative = signs.iter().any(|value| *value < 0.0);
        let positive = signs.iter().any(|value| *value > 0.0);
        if !(negative && positive) {
            return Some([triangle[0], triangle[1], triangle[2]]);
        }
    }
    None
}

fn affine_point(
    rest: &[[f64; 2]],
    deformed: &[[f64; 2]],
    triangle: [usize; 3],
    point: [f64; 2],
) -> Result<[f64; 2], String> {
    let from = mat3_points([rest[triangle[0]], rest[triangle[1]], rest[triangle[2]]]);
    let to = mat3_points([
        deformed[triangle[0]],
        deformed[triangle[1]],
        deformed[triangle[2]],
    ]);
    let mapped = mul_vec3(mul_mat3(to, invert_mat3(from)?), [point[0], point[1], 1.0]);
    if !mapped[0].is_finite() || !mapped[1].is_finite() {
        return Err("non-finite mesh".into());
    }
    Ok([mapped[0], mapped[1]])
}

fn pin_rotation(
    rest: &[[f64; 2]],
    deformed: &[[f64; 2]],
    triangle: [usize; 3],
    point: [f64; 2],
    mapped: [f64; 2],
) -> Result<f64, String> {
    let vertical = affine_point(rest, deformed, triangle, [point[0], point[1] + 1.0])?;
    let horizontal = affine_point(rest, deformed, triangle, [point[0] + 1.0, point[1]])?;
    let vertical_delta =
        (vertical[1] - mapped[1]).atan2(vertical[0] - mapped[0]) - (1.0_f64).atan2(0.0);
    let horizontal_delta =
        (horizontal[1] - mapped[1]).atan2(horizontal[0] - mapped[0]) - (0.0_f64).atan2(1.0);
    Ok((vertical_delta + horizontal_delta) / 2.0)
}

fn singular_triangle(area: f64, corners: [[f64; 2]; 3]) -> bool {
    if !area.is_finite() {
        return true;
    }
    let mut edge = 0.0_f64;
    for i in 0..3 {
        for j in (i + 1)..3 {
            let dx = corners[i][0] - corners[j][0];
            let dy = corners[i][1] - corners[j][1];
            edge = edge.max((dx * dx + dy * dy).sqrt());
        }
    }
    !edge.is_finite() || area.abs() <= 1e-12 * edge * edge
}

fn mat3_points(points: [[f64; 2]; 3]) -> [[f64; 3]; 3] {
    [
        [points[0][0], points[1][0], points[2][0]],
        [points[0][1], points[1][1], points[2][1]],
        [1.0, 1.0, 1.0],
    ]
}

fn mul_mat3(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut result = [[0.0; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            for k in 0..3 {
                result[row][column] += a[row][k] * b[k][column];
            }
        }
    }
    result
}

fn mul_vec3(matrix: [[f64; 3]; 3], point: [f64; 3]) -> [f64; 3] {
    [
        matrix[0][0] * point[0] + matrix[0][1] * point[1] + matrix[0][2] * point[2],
        matrix[1][0] * point[0] + matrix[1][1] * point[1] + matrix[1][2] * point[2],
        matrix[2][0] * point[0] + matrix[2][1] * point[1] + matrix[2][2] * point[2],
    ]
}

fn invert_mat3(matrix: [[f64; 3]; 3]) -> Result<[[f64; 3]; 3], String> {
    let det = matrix[0][0] * (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1])
        - matrix[0][1] * (matrix[1][0] * matrix[2][2] - matrix[1][2] * matrix[2][0])
        + matrix[0][2] * (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]);
    let mut edge = 0.0_f64;
    for i in 0..3 {
        for j in (i + 1)..3 {
            let dx = matrix[0][i] - matrix[0][j];
            let dy = matrix[1][i] - matrix[1][j];
            edge = edge.max((dx * dx + dy * dy).sqrt());
        }
    }
    if !det.is_finite() || !edge.is_finite() || det.abs() <= 1e-12 * edge * edge {
        return Err("non-finite mesh".into());
    }
    let inv_det = 1.0 / det;
    Ok([
        [
            (matrix[1][1] * matrix[2][2] - matrix[1][2] * matrix[2][1]) * inv_det,
            (matrix[0][2] * matrix[2][1] - matrix[0][1] * matrix[2][2]) * inv_det,
            (matrix[0][1] * matrix[1][2] - matrix[0][2] * matrix[1][1]) * inv_det,
        ],
        [
            (matrix[1][2] * matrix[2][0] - matrix[1][0] * matrix[2][2]) * inv_det,
            (matrix[0][0] * matrix[2][2] - matrix[0][2] * matrix[2][0]) * inv_det,
            (matrix[0][2] * matrix[1][0] - matrix[0][0] * matrix[1][2]) * inv_det,
        ],
        [
            (matrix[1][0] * matrix[2][1] - matrix[1][1] * matrix[2][0]) * inv_det,
            (matrix[0][1] * matrix[2][0] - matrix[0][0] * matrix[2][1]) * inv_det,
            (matrix[0][0] * matrix[1][1] - matrix[0][1] * matrix[1][0]) * inv_det,
        ],
    ])
}

fn local_trs(node: &Value, id: u64) -> Result<([f64; 3], [f64; 3], [f64; 2]), String> {
    let transform = node
        .get("transform")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("node {id}.transform must be an object"))?;
    let translation = vector(
        transform.get("trans"),
        3,
        [0.0, 0.0, 0.0],
        &format!("node {id}.transform.trans"),
    )?;
    let rotation = vector(
        transform.get("rot"),
        3,
        [0.0, 0.0, 0.0],
        &format!("node {id}.transform.rot"),
    )?;
    let scale = vector(
        transform.get("scale"),
        2,
        [1.0, 1.0],
        &format!("node {id}.transform.scale"),
    )?;
    Ok((translation, rotation, [scale[0], scale[1]]))
}

fn resolve_mask_bindings<'a>(
    node: &Value,
    id: u64,
    sources: &BTreeMap<u64, MaskSource<'a>>,
) -> Result<Vec<(MaskSource<'a>, MaskBinding)>, String> {
    let bindings = part_mask_bindings(node, id)?;
    let mut seen = HashSet::new();
    Ok(bindings
        .into_iter()
        .filter(|binding| seen.insert((binding.source, binding.mode)))
        .filter_map(|binding| {
            sources
                .get(&binding.source)
                .copied()
                .map(|source| (source, binding))
        })
        .collect())
}

fn part_mask_bindings(node: &Value, id: u64) -> Result<Vec<MaskBinding>, String> {
    if let Some(value) = node.get("masks") {
        let Some(items) = value.as_array() else {
            return if value.is_null() {
                Ok(Vec::new())
            } else {
                Err(format!("Part node {id}.masks must be an array"))
            };
        };
        return items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let path = format!("Part node {id}.masks[{index}]");
                let source = mask_source_id(item.get("source"), &format!("{path}.source"))?;
                let mode = mask_mode(item.get("mode"), &format!("{path}.mode"))?;
                Ok(MaskBinding { source, mode })
            })
            .collect();
    }

    let Some(value) = node.get("masked_by") else {
        return Ok(Vec::new());
    };
    let Some(sources) = value.as_array() else {
        return if value.is_null() {
            Ok(Vec::new())
        } else {
            Err(format!("Part node {id}.masked_by must be an array"))
        };
    };
    if sources.is_empty() {
        return Ok(Vec::new());
    }
    let mode = mask_mode(node.get("mask_mode"), &format!("Part node {id}.mask_mode"))?;
    sources
        .iter()
        .enumerate()
        .map(|(index, source)| {
            Ok(MaskBinding {
                source: mask_source_id(
                    Some(source),
                    &format!("Part node {id}.masked_by[{index}]"),
                )?,
                mode,
            })
        })
        .collect()
}

fn mask_source_id(value: Option<&Value>, path: &str) -> Result<u64, String> {
    value
        .and_then(Value::as_u64)
        .and_then(|id| u32::try_from(id).ok())
        .map(u64::from)
        .ok_or_else(|| format!("{path} must be a UUID"))
}

fn mask_mode(value: Option<&Value>, path: &str) -> Result<MaskMode, String> {
    match value.and_then(Value::as_str) {
        Some("Mask") => Ok(MaskMode::Mask),
        Some("DodgeMask") => Ok(MaskMode::DodgeMask),
        _ => Err(format!("{path} must be Mask or DodgeMask")),
    }
}

#[derive(Clone, Copy)]
struct Material {
    clip_to_destination: bool,
    blend: BlendMode,
    tint: [f32; 3],
    screen_tint: [f32; 3],
    opacity: f32,
    native_tint: bool,
}

fn material(node: &Value, id: u64, kind: &str) -> Result<Material, String> {
    let native_tint = match (node.get("nativeColorBlend"), node.get("nativeAlphaBlend")) {
        (None, None) => false,
        (Some(color), Some(alpha)) => {
            color
                .as_str()
                .ok_or_else(|| format!("{kind} node {id}.nativeColorBlend must be a string"))?;
            alpha
                .as_str()
                .ok_or_else(|| format!("{kind} node {id}.nativeAlphaBlend must be a string"))?;
            true
        }
        (Some(_), None) => {
            return Err(format!("{kind} node {id}.nativeAlphaBlend must be a string"));
        }
        (None, Some(_)) => {
            return Err(format!("{kind} node {id}.nativeColorBlend must be a string"));
        }
    };
    let mode = match node.get("blend_mode") {
        Some(value) => value
            .as_str()
            .ok_or_else(|| format!("{kind} node {id}.blend_mode must be a string"))?,
        None => "Normal",
    };
    let blend = match mode.to_ascii_lowercase().as_str() {
        "cubism53" => BlendMode::Cubism53(crate::native_blend::Mode::parse(
            node["nativeColorBlend"].as_str().ok_or("native color blend is missing")?,
            node["nativeAlphaBlend"].as_str().ok_or("native alpha blend is missing")?,
        )?),
        "normal" => BlendMode::Normal,
        "multiply" => BlendMode::Multiply,
        "screen" => BlendMode::Screen,
        "overlay" => BlendMode::Overlay,
        "darken" => BlendMode::Darken,
        "lighten" => BlendMode::Lighten,
        "colordodge" => BlendMode::ColorDodge,
        "lineardodge" => BlendMode::LinearDodge,
        "addglow" => BlendMode::AddGlow,
        "colorburn" => BlendMode::ColorBurn,
        "hardlight" => BlendMode::HardLight,
        "softlight" => BlendMode::SoftLight,
        "difference" => BlendMode::Difference,
        "exclusion" => BlendMode::Exclusion,
        "subtract" => BlendMode::Subtract,
        "linearburn" => BlendMode::LinearBurn,
        "linearlight" => BlendMode::LinearLight,
        "hue" => BlendMode::Hue,
        "color" => BlendMode::Color,
        "inverse" => BlendMode::Inverse,
        "destinationin" => BlendMode::DestinationIn,
        "cliptolower" => BlendMode::ClipToLower,
        "slicefromlower" => BlendMode::SliceFromLower,
        _ => return Err(format!("{kind} node {id} uses an unsupported blend mode")),
    };
    let blend = match node.get("alphaBlend").map(Value::as_str) {
        None | Some(Some("Over")) => blend,
        Some(Some(alpha @ ("Atop" | "Out" | "Conjoint" | "Disjoint"))) => {
            let color = match blend {
                BlendMode::Normal => "NORMAL",
                BlendMode::LinearDodge => "ADD_R2",
                BlendMode::AddGlow => "ADD_GLOW",
                BlendMode::Darken => "DARKEN",
                BlendMode::Multiply => "MULTIPLY_R2",
                BlendMode::ColorBurn => "COLOR_BURN",
                BlendMode::LinearBurn => "LINEAR_BURN",
                BlendMode::Lighten => "LIGHTEN",
                BlendMode::Screen => "SCREEN",
                BlendMode::ColorDodge => "COLOR_DODGE",
                BlendMode::Overlay => "OVERLAY",
                BlendMode::SoftLight => "SOFTLIGHT",
                BlendMode::HardLight => "HARDLIGHT",
                BlendMode::LinearLight => "LINEAR_LIGHT",
                BlendMode::Hue => "HSL_HUE",
                BlendMode::Color => "HSL_COLOR",
                other => {
                    return Err(format!(
                        "{kind} node {id} blends as {}, which has no Cubism alpha blend; alphaBlend must be Over",
                        other.canonical()
                    ))
                }
            };
            BlendMode::Cubism53(crate::native_blend::Mode::parse(color, &alpha.to_ascii_uppercase())?)
        }
        Some(_) => return Err(format!("{kind} node {id}.alphaBlend must be Over, Atop, Out, Conjoint or Disjoint")),
    };
    let tint = vector(
        node.get("tint"),
        3,
        [1.0; 3],
        &format!("{kind} node {id}.tint"),
    )?;
    let screen_tint = vector(
        node.get("screenTint"),
        3,
        [0.0; 3],
        &format!("{kind} node {id}.screenTint"),
    )?;
    let opacity = optional_finite(
        node.get("opacity"),
        1.0,
        &format!("{kind} node {id}.opacity"),
    )?
    .clamp(0.0, 1.0) as f32;
    Ok(Material {
        clip_to_destination: false,
        blend,
        tint: tint.map(|component| component.clamp(0.0, 1.0) as f32),
        screen_tint: screen_tint.map(|component| component.clamp(0.0, 1.0) as f32),
        opacity,
        native_tint,
    })
}

/// Refuses a `blend_mode` and `alphaBlend` pair that `render` cannot draw.
pub(crate) fn check_blend(node: &Value, id: u64, kind: &str) -> Result<(), String> {
    material(node, id, kind).map(|_| ())
}

/// Draws the mask bindings of one node into `stencil`.
fn fill_mask_stencil(
    bindings: Vec<(MaskSource<'_>, MaskBinding)>,
    scene: &Scene<'_>,
    stencil: &mut [u8],
    context: &mut RenderContext<'_>,
) -> Result<(), String> {
    let has_normal_mask = bindings
        .iter()
        .any(|(_, binding)| binding.mode == MaskMode::Mask);
    stencil.fill(if has_normal_mask { 0 } else { 1 });
    for (source, binding) in bindings {
        let write = if binding.mode == MaskMode::Mask { 1 } else { 0 };
        raster_mask_source(source, scene, stencil, write, context)?;
    }
    Ok(())
}

/// Leaves the mask of `node` in `stencil`; false when the node has none. `drawn` holds the bindings the stencil
/// was last filled for and is checked first: layers clipped to one base are adjacent and ask for the same mask, so
/// the base is rasterised once for the run.
fn prepare_mask_stencil(
    node: &Value,
    id: u64,
    scene: &Scene<'_>,
    stencil: &mut [u8],
    drawn: &mut Vec<(u64, MaskMode)>,
    context: &mut RenderContext<'_>,
) -> Result<bool, String> {
    let bindings = resolve_mask_bindings(node, id, &scene.sources)?;
    if bindings.is_empty() || context.psd_clip_stacks {
        return Ok(false);
    }
    let key: Vec<_> = bindings
        .iter()
        .map(|(_, binding)| (binding.source, binding.mode))
        .collect();
    if *drawn != key {
        fill_mask_stencil(bindings, scene, stencil, context)?;
        *drawn = key;
    }
    Ok(true)
}

fn render_scope(
    scene: &Scene<'_>,
    scope_index: usize,
    canvas: &mut [f32],
    mut coverage: Option<&mut [u8]>,
    context: &mut RenderContext<'_>,
) -> Result<PixelBox, String> {
    let scope = scene
        .scopes
        .get(scope_index)
        .ok_or_else(|| "render scope index is invalid".to_string())?;
    context.with_stencil(|stencil, context| {
        let mut drawn = Vec::new();
        let mut dirty = None;
        for item in scope.items.iter().copied() {
            let draw = item.draw();
            let id = draw.node["uuid"].as_u64().unwrap_or(0);
            let skip = match context.solo {
                None => false,
                // The set holds the nodes below a solo node too, so a Composite in it is solo as a whole.
                Some(SoloPass::Rest(only)) => only.contains(&id),
                Some(SoloPass::Only(only)) => !holds_solo(scene, item, only),
            };
            if skip {
                continue;
            }
            let written = match item {
                RenderItem::Part(_) => {
                    let has_stencil = prepare_mask_stencil(draw.node, id, scene, stencil, &mut drawn, context)?;
                    raster_part(
                        draw,
                        canvas,
                        coverage.as_deref_mut(),
                        has_stencil.then_some(&*stencil),
                        context,
                    )?
                }
                RenderItem::Composite { scope, .. } => {
                    render_composite(draw, scope, scene, canvas, coverage.as_deref_mut(), context)?
                }
            };
            dirty = union_box(dirty, written);
        }
        Ok(dirty)
    })
}

/// The item draws in the Solo view: a Part in `only`, or a Composite in it or holding a Part or Composite that is.
fn holds_solo(scene: &Scene<'_>, item: RenderItem<'_>, only: &HashSet<u64>) -> bool {
    let id = item.draw().node["uuid"].as_u64().unwrap_or(0);
    only.contains(&id)
        || match item {
            RenderItem::Part(_) => false,
            RenderItem::Composite { scope, .. } => scene
                .scopes
                .get(scope)
                .is_some_and(|scope| scope.items.iter().any(|inner| holds_solo(scene, *inner, only))),
        }
}

/// Sets the coverage of the box around the covered pixels and returns that box. `dirty` holds every covered pixel.
fn expand_coverage_to_bounds(
    coverage: &mut [u8],
    width: u32,
    dirty: PixelBox,
    context: &mut RenderContext<'_>,
) -> Result<PixelBox, String> {
    context.charge_work(box_area(dirty))?;
    let mut covered = None;
    for pixel in box_pixels(dirty, width) {
        if coverage[pixel] != 0 {
            let (x, y) = (pixel as u32 % width, pixel as u32 / width);
            covered = union_box(covered, Some([x, y, x, y]));
        }
    }
    context.charge_work(box_area(covered))?;
    for pixel in box_pixels(covered, width) {
        coverage[pixel] = 1;
    }
    Ok(covered)
}

fn composite_source_bounds(
    draw: Draw<'_>,
    deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
    world_matrices: &BTreeMap<u32, Mat4>,
    raster_work: &mut u64,
) -> Result<Option<[f64; 4]>, String> {
    fn include(bounds: &mut [f64; 4], point: [f64; 2]) -> Result<(), String> {
        if point.iter().any(|value| !value.is_finite()) {
            return Err("Composite bounds are not finite".to_string());
        }
        bounds[0] = bounds[0].min(point[0]);
        bounds[1] = bounds[1].min(point[1]);
        bounds[2] = bounds[2].max(point[0]);
        bounds[3] = bounds[3].max(point[1]);
        Ok(())
    }

    fn include_rect(
        bounds: &mut [f64; 4],
        rect: [f64; 4],
        raster_work: &mut u64,
    ) -> Result<(), String> {
        for point in [
            [rect[0], rect[1]],
            [rect[0], rect[3]],
            [rect[2], rect[1]],
            [rect[2], rect[3]],
        ] {
            charge_raster_work(raster_work, 1)?;
            include(bounds, point)?;
        }
        Ok(())
    }

    fn empty_bounds() -> [f64; 4] {
        [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ]
    }

    fn visit(
        node: &Value,
        deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
        world_matrices: &BTreeMap<u32, Mat4>,
        raster_work: &mut u64,
        raw_bounds: &mut [f64; 4],
        candidate_bounds: &mut [f64; 4],
        composite_depth: usize,
    ) -> Result<(), String> {
        let node_type = node.get("type").and_then(Value::as_str).unwrap_or("Node");
        if is_composite_type(node_type) {
            if !node.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
                return Ok(());
            }
            let id = node
                .get("uuid")
                .and_then(Value::as_u64)
                .ok_or("Composite node uuid must be an unsigned integer")?;
            let Some(nested_bounds) = bounds_for(
                node,
                deformations,
                world_matrices,
                raster_work,
                composite_depth + 1,
            )?
            else {
                return Ok(());
            };

            include_rect(raw_bounds, nested_bounds, raster_work)?;

            // D bounds each nested quad from its transformed origin and current
            // deformed corners. Keep these candidates separate until all
            // siblings have contributed to the parent bounds.
            let matrix = u32::try_from(id)
                .ok()
                .and_then(|id| world_matrices.get(&id))
                .copied()
                .ok_or_else(|| format!("Composite node {id} has no evaluated world matrix"))?;
            charge_raster_work(raster_work, 1)?;
            let origin = project(matrix, [0.0, 0.0])?;
            include(candidate_bounds, origin)?;
            let offsets = composite_offsets(node, id, deformations)?.unwrap_or([[0.0; 2]; 4]);
            for point in composite_output_corners(id, nested_bounds, offsets)? {
                charge_raster_work(raster_work, 1)?;
                include(candidate_bounds, point)?;
            }
            return Ok(());
        }
        if node_type == "Part" {
            let id = node
                .get("uuid")
                .and_then(Value::as_u64)
                .ok_or("Part node uuid must be an unsigned integer")?;
            let matrix = u32::try_from(id)
                .ok()
                .and_then(|id| world_matrices.get(&id))
                .copied()
                .ok_or_else(|| format!("Part node {id} has no evaluated world matrix"))?;
            let mut arrays = load_part_arrays(node, id)?
                .ok_or_else(|| format!("Part node {id}.mesh must be an object"))?;
            apply_deformations(&mut arrays.vertices, id, deformations)?;
            charge_raster_work(raster_work, 1)?;
            let origin = project(matrix, [0.0, 0.0])?;
            include(raw_bounds, origin)?;
            include(candidate_bounds, origin)?;
            for vertex in arrays.vertices {
                charge_raster_work(raster_work, 1)?;
                let point = project(matrix, vertex)?;
                include(raw_bounds, point)?;
                include(candidate_bounds, point)?;
            }
        } else if !node.get("enabled").and_then(Value::as_bool).unwrap_or(true) {
            return Ok(());
        }
        let Some(children) = node.get("children") else {
            return Ok(());
        };
        let children = children
            .as_array()
            .ok_or_else(|| "node.children must be an array".to_string())?;
        for child in children {
            visit(
                child,
                deformations,
                world_matrices,
                raster_work,
                raw_bounds,
                candidate_bounds,
                composite_depth,
            )?;
        }
        Ok(())
    }

    fn bounds_for(
        node: &Value,
        deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
        world_matrices: &BTreeMap<u32, Mat4>,
        raster_work: &mut u64,
        composite_depth: usize,
    ) -> Result<Option<[f64; 4]>, String> {
        if composite_depth >= MAX_COMPOSITE_DEPTH {
            return Err("render exceeds the 16-level Composite nesting limit".to_string());
        }
        let mut raw_bounds = empty_bounds();
        let mut candidate_bounds = empty_bounds();
        if let Some(children) = node.get("children") {
            let children = children
                .as_array()
                .ok_or_else(|| "Composite node children must be an array".to_string())?;
            for child in children {
                visit(
                    child,
                    deformations,
                    world_matrices,
                    raster_work,
                    &mut raw_bounds,
                    &mut candidate_bounds,
                    composite_depth,
                )?;
            }
        }
        if raw_bounds[0] == f64::INFINITY {
            return Ok(None);
        }
        if raw_bounds.iter().any(|value| !value.is_finite())
            || candidate_bounds.iter().any(|value| !value.is_finite())
        {
            return Err("Composite bounds are not finite".to_string());
        }
        let current_width = raw_bounds[2] - raw_bounds[0];
        let current_height = raw_bounds[3] - raw_bounds[1];
        let candidate_width = candidate_bounds[2] - candidate_bounds[0];
        let candidate_height = candidate_bounds[3] - candidate_bounds[1];
        let width = if candidate_width.trunc() > current_width.trunc() {
            candidate_width
        } else {
            current_width
        };
        let height = if candidate_height.trunc() > current_height.trunc() {
            candidate_height
        } else {
            current_height
        };
        let center = [
            raw_bounds[0] + current_width / 2.0,
            raw_bounds[1] + current_height / 2.0,
        ];
        let generated_bounds = [
            center[0] - width / 2.0,
            center[1] - height / 2.0,
            center[0] + width / 2.0,
            center[1] + height / 2.0,
        ];
        if generated_bounds.iter().any(|value| !value.is_finite()) {
            return Err("Composite bounds are not finite".to_string());
        }
        Ok(Some(generated_bounds))
    }

    bounds_for(draw.node, deformations, world_matrices, raster_work, 0)
}

fn composite_output_corners(
    id: u64,
    bounds: [f64; 4],
    offsets: [[f64; 2]; 4],
) -> Result<[[f64; 2]; 4], String> {
    let corners = [
        [bounds[0], bounds[1]],
        [bounds[0], bounds[3]],
        [bounds[2], bounds[1]],
        [bounds[2], bounds[3]],
    ];
    let output = std::array::from_fn(|index| {
        [
            corners[index][0] + offsets[index][0],
            corners[index][1] + offsets[index][1],
        ]
    });
    if output.iter().flatten().any(|value| !value.is_finite()) {
        return Err(format!("Composite node {id} has invalid deformed vertices"));
    }
    Ok(output)
}

fn composite_source_surface(id: u64, bounds: [f64; 4]) -> Result<(u32, u32, [f64; 4]), String> {
    let extent_x = bounds[2] - bounds[0];
    let extent_y = bounds[3] - bounds[1];
    let surface_width = extent_x.ceil() + 1.0;
    let surface_height = extent_y.ceil() + 1.0;
    if !surface_width.is_finite()
        || !surface_height.is_finite()
        || surface_width < 1.0
        || surface_height < 1.0
        || surface_width > f64::from(u32::MAX)
        || surface_height > f64::from(u32::MAX)
    {
        return Err(format!(
            "Composite node {id} source bounds exceed the raster limits"
        ));
    }
    let width = surface_width as u32;
    let height = surface_height as u32;
    let pixels = u64::from(width)
        .checked_mul(u64::from(height))
        .ok_or_else(|| format!("Composite node {id} source dimensions overflow"))?;
    if pixels > MAX_IMAGE_PIXELS {
        return Err(format!(
            "Composite node {id} source layer exceeds the 16-megapixel limit"
        ));
    }
    let center_x = bounds[0] + extent_x / 2.0;
    let center_y = bounds[1] + extent_y / 2.0;
    let left = center_x - f64::from(width) / 2.0;
    let top = center_y - f64::from(height) / 2.0;
    let right = left + f64::from(width);
    let bottom = top + f64::from(height);
    if [extent_x, extent_y, left, top, right, bottom]
        .iter()
        .any(|value| !value.is_finite())
        || extent_x < 0.0
        || extent_y < 0.0
        || right <= left
        || bottom <= top
    {
        return Err(format!(
            "Composite node {id} source bounds exceed the raster limits"
        ));
    }
    Ok((width, height, [left, top, right, bottom]))
}

fn quantize_composite_texture(pixels: &mut [f32]) {
    for component in pixels {
        *component = (component.clamp(0.0, 1.0) * 255.0).round() / 255.0;
    }
}

fn warp_composite_layer(
    id: u64,
    source: &[f32],
    source_width: u32,
    source_height: u32,
    offsets: [[f64; 2]; 4],
    bounds: [f64; 4],
    layer: &mut [f32],
    coverage: &mut [u8],
    context: &mut RenderContext<'_>,
) -> Result<PixelBox, String> {
    let screen = composite_output_corners(id, bounds, offsets)?
        .into_iter()
        .map(|world| {
            let point = [
                (world[0] - context.bounds[0]) * f64::from(context.width)
                    / (context.bounds[2] - context.bounds[0]),
                (world[1] - context.bounds[1]) * f64::from(context.height)
                    / (context.bounds[3] - context.bounds[1]),
            ];
            if world.iter().any(|value| !value.is_finite())
                || point
                    .iter()
                    .any(|value| !value.is_finite() || value.abs() > 1e9)
            {
                return Err(format!("Composite node {id} has invalid deformed vertices"));
            }
            Ok(point)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let uvs = [[0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]];
    let mut dirty = None;
    for indices in [[0, 1, 2], [2, 1, 3]] {
        let written = raster_triangle(
            [screen[indices[0]], screen[indices[1]], screen[indices[2]]],
            RasterTarget::Layer {
                uvs: [uvs[indices[0]], uvs[indices[1]], uvs[indices[2]]],
                source,
                source_width,
                source_height,
                canvas: &mut *layer,
                coverage: Some(&mut *coverage),
            },
            context.width,
            context.height,
            &mut context.raster_work,
            id,
        )?;
        dirty = union_box(dirty, written);
    }
    Ok(dirty)
}

fn with_composite_layer<T>(
    draw: Draw<'_>,
    scope: usize,
    scene: &Scene<'_>,
    context: &mut RenderContext<'_>,
    apply_outer_masks: bool,
    operation: impl FnOnce(&[f32], &[u8], PixelBox, Material, &mut RenderContext<'_>) -> Result<T, String>,
) -> Result<T, String> {
    let id = draw.node["uuid"].as_u64().unwrap_or(0);
    let mut material = material(draw.node, id, "Composite")?;
    material.clip_to_destination = context.psd_clipped_nodes.contains(&id);
    let offsets = composite_offsets(draw.node, id, context.deformations)?;
    let needs_warp =
        offsets.is_some_and(|offsets| offsets.iter().any(|offset| *offset != [0.0; 2]));
    let source_bounds = if needs_warp {
        composite_source_bounds(
            draw,
            context.deformations,
            context.world_matrices,
            &mut context.raster_work,
        )?
    } else {
        None
    };
    context.enter_composite()?;
    let result = context.with_scratch_layer(|layer, coverage, context| {
        let written = if let (Some(offsets), Some(bounds)) = (offsets.filter(|_| needs_warp), source_bounds) {
            let (source_width, source_height, source_surface) =
                composite_source_surface(id, bounds)?;
            let source_pixels = usize::try_from(u64::from(source_width) * u64::from(source_height))
                .map_err(|_| format!("Composite node {id} source dimensions overflow"))?;
            let source_bytes = (source_pixels as u64)
                .checked_mul(4 * std::mem::size_of::<f32>() as u64 + 1)
                .ok_or_else(|| format!("Composite node {id} source memory size overflow"))?;
            context.charge_work(source_pixels as u64)?;
            context.reserve_memory(source_bytes)?;
            let mut source = vec![0.0; source_pixels * 4];
            let mut source_coverage = vec![0; source_pixels];
            let viewport = (
                context.width,
                context.height,
                context.bounds,
                context.pixel_count,
            );
            context.width = source_width;
            context.height = source_height;
            context.bounds = source_surface;
            context.pixel_count = source_pixels;
            let render_result = render_scope(
                scene,
                scope,
                &mut source,
                Some(&mut source_coverage),
                context,
            );
            context.width = viewport.0;
            context.height = viewport.1;
            context.bounds = viewport.2;
            context.pixel_count = viewport.3;
            let result = render_result.and_then(|_| {
                quantize_composite_texture(&mut source);
                warp_composite_layer(
                    id,
                    &source,
                    source_width,
                    source_height,
                    offsets,
                    bounds,
                    layer,
                    coverage,
                    context,
                )
            });
            context.release_memory(source_bytes);
            result?
        } else {
            let dirty = render_scope(scene, scope, layer, Some(coverage), context)?;
            for pixel in box_pixels(dirty, context.width) {
                quantize_composite_texture(&mut layer[pixel * 4..pixel * 4 + 4]);
            }
            expand_coverage_to_bounds(coverage, context.width, dirty, context)?
        };
        let mut operation = Some(operation);
        let mut apply = |stencil: Option<&[u8]>, context: &mut RenderContext<'_>| {
            if let Some(stencil) = stencil {
                for pixel in box_pixels(written, context.width) {
                    if stencil[pixel] != 1 {
                        coverage[pixel] = 0;
                    }
                }
            }
            operation.take().expect("composite operation runs once")(
                layer, coverage, written, material, context,
            )
        };
        let bindings = if apply_outer_masks && !context.psd_clip_stacks {
            resolve_mask_bindings(draw.node, id, &scene.sources)?
        } else {
            Vec::new()
        };
        if bindings.is_empty() {
            apply(None, context)
        } else {
            context.with_stencil(|stencil, context| {
                fill_mask_stencil(bindings, scene, stencil, context)?;
                apply(Some(stencil), context)
            })
        }
    });
    context.leave_composite();
    result
}

fn render_composite(
    draw: Draw<'_>,
    scope: usize,
    scene: &Scene<'_>,
    destination: &mut [f32],
    mut destination_coverage: Option<&mut [u8]>,
    context: &mut RenderContext<'_>,
) -> Result<PixelBox, String> {
    let width = context.width;
    with_composite_layer(
        draw,
        scope,
        scene,
        context,
        true,
        |layer, coverage, written, material, _| {
            for pixel in box_pixels(written, width) {
                if coverage[pixel] == 0 {
                    continue;
                }
                let offset = pixel * 4;
                let source = composite_source(
                    [
                        layer[offset],
                        layer[offset + 1],
                        layer[offset + 2],
                        layer[offset + 3],
                    ],
                    material,
                );
                let destination_pixel = [
                    destination[offset],
                    destination[offset + 1],
                    destination[offset + 2],
                    destination[offset + 3],
                ];
                if let Some(stencil) = destination_coverage.as_deref_mut() {
                    stencil[pixel] = 1;
                }
                destination[offset..offset + 4].copy_from_slice(&blend_material(
                    destination_pixel,
                    source,
                    material,
                ));
            }
            Ok(written)
        },
    )
}

fn composite_source(pixel: [f32; 4], material: Material) -> [f32; 4] {
    material_color(pixel, material)
}

fn material_color(pixel: [f32; 4], material: Material) -> [f32; 4] {
    let alpha = pixel[3];
    let rgb: [f32; 3] = if material.native_tint {
        std::array::from_fn(|index| {
            let multiplied = pixel[index] * material.tint[index];
            (multiplied + material.screen_tint[index] * alpha
                - multiplied * material.screen_tint[index])
                * material.opacity
        })
    } else {
        std::array::from_fn(|index| {
            (1.0 - (1.0 - pixel[index]) * (1.0 - material.screen_tint[index] * alpha))
                * material.tint[index]
                * material.opacity
        })
    };
    [
        rgb[0],
        rgb[1],
        rgb[2],
        alpha * material.opacity,
    ]
}

fn raster_mask_source(
    source: MaskSource<'_>,
    scene: &Scene<'_>,
    stencil: &mut [u8],
    value: u8,
    context: &mut RenderContext<'_>,
) -> Result<(), String> {
    let draw = source.draw;
    let node = draw.node;
    let id = node["uuid"].as_u64().unwrap_or(0);
    if source.kind == SourceKind::Composite {
        let scope = source
            .scope
            .ok_or_else(|| format!("Composite node {id} has no render scope"))?;
        if !context.active_composite_masks.insert(id) {
            return Err(format!("Composite node {id} has a cyclic mask dependency"));
        }
        let threshold = optional_finite(
            node.get("mask_threshold"),
            0.5,
            &format!("Composite node {id}.mask_threshold"),
        )?
        .clamp(0.0, 1.0) as f32;
        let width = context.width;
        // A mask is whole whatever the Solo view draws.
        let pass = context.solo.take();
        let result = with_composite_layer(
            draw,
            scope,
            scene,
            context,
            false,
            |layer, coverage, written, _, _| {
                for pixel in box_pixels(written, width) {
                    if coverage[pixel] != 0 && layer[pixel * 4 + 3] > threshold {
                        stencil[pixel] = value;
                    }
                }
                Ok(())
            },
        );
        context.active_composite_masks.remove(&id);
        context.solo = pass;
        return result;
    }
    let mut arrays = load_part_arrays(node, id)?
        .ok_or_else(|| format!("Part node {id}.mesh must be an object"))?;
    apply_deformations(&mut arrays.vertices, id, context.deformations)?;
    let indices = arrays.indices;
    if indices.len() % 3 != 0 {
        return Err(format!(
            "Part node {id}.mesh.indices must contain triangles"
        ));
    }
    if indices.len() as u64 / 3 > 5_000_000 {
        return Err(format!("Part node {id} exceeds the triangle limit"));
    }

    let alpha_source = if matches!(source.kind, SourceKind::Part) {
        let texture_index = first_albedo_slot(node, id, !indices.is_empty())?;
        let Some(texture_index) = texture_index else {
            return Ok(());
        };
        let uvs = arrays
            .uvs
            .take()
            .ok_or_else(|| format!("Part node {id}.mesh.uvs must be a flattened numeric array"))?;
        if arrays.vertices.len() != uvs.len() {
            return Err(format!(
                "Part node {id} has mismatched vertex and UV counts"
            ));
        }
        if uvs.iter().flatten().any(|uv| !uv.is_finite()) {
            return Err(format!(
                "Part node {id} contains invalid transformed vertex or UV data"
            ));
        }
        context.ensure_texture(id, texture_index)?;
        let texture = context
            .decoded_textures
            .get(&texture_index)
            .expect("texture was loaded");
        let threshold = optional_finite(
            node.get("mask_threshold"),
            0.5,
            &format!("Part node {id}.mask_threshold"),
        )?
        .clamp(0.0, 1.0) as f32;
        Some((uvs, texture, threshold))
    } else {
        None
    };
    let screen_vertices = screen_vertices(
        id,
        &arrays.vertices,
        arrays.origin,
        draw.matrix,
        context.welded,
        context.width,
        context.height,
        context.bounds,
    )?;
    for triangle in indices.chunks_exact(3) {
        let ids = checked_triangle_ids(triangle, arrays.vertices.len(), id)?;
        let alpha = alpha_source
            .as_ref()
            .map(|(uvs, texture, threshold)| AlphaTest {
                uvs: [uvs[ids[0]], uvs[ids[1]], uvs[ids[2]]],
                texture,
                threshold: *threshold,
            });
        raster_triangle(
            [
                screen_vertices[ids[0]],
                screen_vertices[ids[1]],
                screen_vertices[ids[2]],
            ],
            RasterTarget::Stencil {
                stencil: &mut *stencil,
                value,
                alpha,
            },
            context.width,
            context.height,
            &mut context.raster_work,
            id,
        )?;
    }
    Ok(())
}

fn raster_part(
    draw: Draw<'_>,
    canvas: &mut [f32],
    mut coverage: Option<&mut [u8]>,
    stencil: Option<&[u8]>,
    context: &mut RenderContext<'_>,
) -> Result<PixelBox, String> {
    let node = draw.node;
    let id = node["uuid"].as_u64().unwrap_or(0);
    let mut material = material(node, id, "Part")?;
    material.clip_to_destination = context.psd_clipped_nodes.contains(&id);
    optional_finite(
        node.get("emissionStrength"),
        1.0,
        &format!("Part node {id}.emissionStrength"),
    )?;
    validate_auxiliary_textures(node, id, context)?;
    let mut arrays = load_part_arrays(node, id)?
        .ok_or_else(|| format!("Part node {id}.mesh must be an object"))?;
    apply_deformations(&mut arrays.vertices, id, context.deformations)?;
    if arrays.indices.len() % 3 != 0 {
        return Err(format!(
            "Part node {id}.mesh.indices must contain triangles"
        ));
    }
    let texture_index = albedo_slot(node, id, !arrays.indices.is_empty())?;
    let Some(texture_index) = texture_index else {
        return Ok(None);
    };
    let uvs = arrays
        .uvs
        .take()
        .ok_or_else(|| format!("Part node {id}.mesh.uvs must be a flattened numeric array"))?;
    if arrays.vertices.len() != uvs.len() {
        return Err(format!(
            "Part node {id} has mismatched vertex and UV counts"
        ));
    }
    context.ensure_texture(id, texture_index)?;
    // A native texture whose minFilter names a mipmap mode (`LINEAR_MIPMAP_LINEAR` in every sample CMO3) is
    // minified through its mip chain; every other texture samples its base level.
    if node.get("textureMipmaps") == Some(&Value::Bool(true)) {
        context.ensure_mips(id, texture_index)?;
    }
    let texture = context
        .decoded_textures
        .get(&texture_index)
        .expect("texture was loaded");
    let mips = context.mip_chains.get(&texture_index).filter(|_| node.get("textureMipmaps") == Some(&Value::Bool(true)));
    if arrays.indices.len() as u64 / 3 > 5_000_000 {
        return Err(format!("Part node {id} exceeds the triangle limit"));
    }
    if uvs.iter().flatten().any(|uv| !uv.is_finite()) {
        return Err(format!(
            "Part node {id} contains invalid transformed vertex or UV data"
        ));
    }
    let screen_vertices = screen_vertices(
        id,
        &arrays.vertices,
        arrays.origin,
        draw.matrix,
        context.welded,
        context.width,
        context.height,
        context.bounds,
    )?;
    let mut dirty = None;
    for triangle in arrays.indices.chunks_exact(3) {
        let ids = checked_triangle_ids(triangle, arrays.vertices.len(), id)?;
        let written = raster_triangle(
            [
                screen_vertices[ids[0]],
                screen_vertices[ids[1]],
                screen_vertices[ids[2]],
            ],
            RasterTarget::Color {
                uvs: [uvs[ids[0]], uvs[ids[1]], uvs[ids[2]]],
                texture,
                mips: mips.map_or(&[][..], Vec::as_slice),
                material,
                canvas: &mut *canvas,
                coverage: coverage.as_deref_mut(),
                stencil,
            },
            context.width,
            context.height,
            &mut context.raster_work,
            id,
        )?;
        dirty = union_box(dirty, written);
    }
    Ok(dirty)
}

fn apply_deformations(
    vertices: &mut [[f64; 2]],
    id: u64,
    deformations: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<(), String> {
    if let Some(offsets) = u32::try_from(id).ok().and_then(|id| deformations.get(&id)) {
        if offsets.len() != vertices.len() {
            return Err(format!("Part node {id} deform offset length mismatch"));
        }
        for (vertex, offset) in vertices.iter_mut().zip(offsets) {
            vertex[0] += offset[0];
            vertex[1] += offset[1];
            if !vertex[0].is_finite() || !vertex[1].is_finite() {
                return Err(format!("Part node {id} deformed vertex is not finite"));
            }
        }
    }
    Ok(())
}

fn screen_vertices(
    id: u64,
    vertices: &[[f64; 2]],
    origin: [f64; 2],
    matrix: Mat4,
    welded: &HashMap<(u32, usize), [f64; 2]>,
    width: u32,
    height: u32,
    bounds: [f64; 4],
) -> Result<Vec<[f64; 2]>, String> {
    world_vertices(id, vertices, origin, matrix, welded)?
        .into_iter()
        .map(|world| {
            let screen = [
                (world[0] - bounds[0]) * f64::from(width) / (bounds[2] - bounds[0]),
                (world[1] - bounds[1]) * f64::from(height) / (bounds[3] - bounds[1]),
            ];
            if world.iter().any(|value| !value.is_finite())
                || screen
                    .iter()
                    .any(|value| !value.is_finite() || value.abs() > 1e9)
            {
                return Err(format!(
                    "Part node {id} contains invalid transformed vertex data"
                ));
            }
            Ok(screen)
        })
        .collect()
}

fn world_vertices(
    id: u64,
    vertices: &[[f64; 2]],
    origin: [f64; 2],
    matrix: Mat4,
    welded: &HashMap<(u32, usize), [f64; 2]>,
) -> Result<Vec<[f64; 2]>, String> {
    vertices
        .iter()
        .enumerate()
        .map(|(index, vertex)| {
            let local = [vertex[0] - origin[0], vertex[1] - origin[1]];
            let welded_point = u32::try_from(id)
                .ok()
                .and_then(|id| welded.get(&(id, index)).copied());
            let world = match welded_point {
                Some(point) => point,
                None => project(matrix, local)?,
            };
            if world.iter().any(|value| !value.is_finite()) {
                return Err(format!(
                    "Part node {id} contains invalid transformed vertex data"
                ));
            }
            Ok(world)
        })
        .collect()
}

fn checked_triangle_ids(
    triangle: &[usize],
    vertex_count: usize,
    id: u64,
) -> Result<[usize; 3], String> {
    let ids = [triangle[0], triangle[1], triangle[2]];
    if ids.iter().any(|index| *index >= vertex_count) {
        return Err(format!("Part node {id} has an out-of-range mesh index"));
    }
    Ok(ids)
}

fn albedo_slot(node: &Value, id: u64, has_triangles: bool) -> Result<Option<usize>, String> {
    first_albedo_slot(node, id, has_triangles)
}

fn validate_auxiliary_textures(
    node: &Value,
    id: u64,
    context: &mut RenderContext<'_>,
) -> Result<(), String> {
    let Some(value) = node.get("textures") else {
        return Ok(());
    };
    let slots = value
        .as_array()
        .ok_or_else(|| format!("Part node {id}.textures must be an array"))?;
    for (index, slot) in slots.iter().enumerate().skip(1) {
        let Some(texture_index) = texture_slot(slot, id, index)? else {
            continue;
        };
        if index > 2 {
            return Err(format!(
                "Part node {id} uses unsupported texture slot {index}"
            ));
        }
        context.ensure_texture(id, texture_index)?;
    }
    Ok(())
}

fn first_albedo_slot(node: &Value, id: u64, has_triangles: bool) -> Result<Option<usize>, String> {
    let Some(value) = node.get("textures") else {
        if has_triangles {
            return Err(format!("Part node {id} is missing its texture slots"));
        }
        return Ok(None);
    };
    let slots = value
        .as_array()
        .ok_or_else(|| format!("Part node {id}.textures must be an array"))?;
    match slots.first() {
        Some(slot) => texture_slot(slot, id, 0),
        None if has_triangles => Err(format!("Part node {id} has no albedo texture slot")),
        None => Ok(None),
    }
}

fn texture_slot(slot: &Value, id: u64, position: usize) -> Result<Option<usize>, String> {
    if slot.is_null() || slot.as_u64() == Some(NO_TEXTURE) {
        return Ok(None);
    }
    slot.as_u64()
        .and_then(|slot| usize::try_from(slot).ok())
        .map(Some)
        .ok_or_else(|| format!("Part node {id}.textures[{position}] must be a texture slot index"))
}

pub(crate) fn decode_texture(blob: &TextureBlob, slot: usize) -> Result<CpuTexture, String> {
    let mut texture = decode_stored_texture(blob, slot)?;
    // Textures are drawn as premultiplied, where a fully transparent texel has no colour. Colour bleed
    // (`tool.rebleed_textures`) stores some there for other consumers; drawn as is it would add a halo.
    for pixel in texture.rgba.chunks_exact_mut(4).filter(|pixel| pixel[3] == 0) {
        pixel[..3].fill(0);
    }
    Ok(texture)
}

/// The pixels of a texture as they are stored, with whatever colour its fully transparent texels hold.
pub(crate) fn decode_stored_texture(blob: &TextureBlob, slot: usize) -> Result<CpuTexture, String> {
    let format = match blob.tag {
        0 => ImageFormat::Png,
        1 => ImageFormat::Tga,
        tag => {
            return Err(format!(
                "texture slot {slot} has unsupported image tag {tag}"
            ))
        }
    };
    let mut reader = ImageReader::with_format(Cursor::new(blob.data.as_slice()), format);
    let mut limits = Limits::default();
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    limits.max_alloc = Some(64 * 1024 * 1024);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|error| format!("could not decode texture slot {slot}: {error}"))?
        .into_rgba8();
    let pixels = u64::from(image.width()) * u64::from(image.height());
    if image.width() == 0 || image.height() == 0 || pixels > MAX_TEXTURE_PIXELS {
        return Err(format!(
            "texture slot {slot} exceeds the 16-megapixel limit"
        ));
    }
    let (width, height) = (image.width(), image.height());
    Ok(CpuTexture { width, height, rgba: image.into_raw() })
}

struct AlphaTest<'a> {
    uvs: [[f64; 2]; 3],
    texture: &'a CpuTexture,
    threshold: f32,
}

enum RasterTarget<'a> {
    Color {
        uvs: [[f64; 2]; 3],
        texture: &'a CpuTexture,
        /// Levels 1 and below of `texture`; empty samples the base level only.
        mips: &'a [CpuTexture],
        material: Material,
        canvas: &'a mut [f32],
        coverage: Option<&'a mut [u8]>,
        stencil: Option<&'a [u8]>,
    },
    Stencil {
        stencil: &'a mut [u8],
        value: u8,
        alpha: Option<AlphaTest<'a>>,
    },
    Layer {
        uvs: [[f64; 2]; 3],
        source: &'a [f32],
        source_width: u32,
        source_height: u32,
        canvas: &'a mut [f32],
        coverage: Option<&'a mut [u8]>,
    },
}

#[allow(clippy::too_many_arguments)]
fn raster_triangle(
    vertices: [[f64; 2]; 3],
    mut target: RasterTarget<'_>,
    width: u32,
    height: u32,
    raster_work: &mut u64,
    node_id: u64,
) -> Result<PixelBox, String> {
    let area = edge(vertices[0], vertices[1], vertices[2]);
    if !area.is_finite() {
        return Err(format!("Part node {node_id} has an unbounded triangle"));
    }
    if area.abs() <= f64::EPSILON {
        return Ok(None);
    }
    let min_x = vertices
        .iter()
        .map(|point| point[0])
        .fold(f64::INFINITY, f64::min);
    let max_x = vertices
        .iter()
        .map(|point| point[0])
        .fold(f64::NEG_INFINITY, f64::max);
    let min_y = vertices
        .iter()
        .map(|point| point[1])
        .fold(f64::INFINITY, f64::min);
    let max_y = vertices
        .iter()
        .map(|point| point[1])
        .fold(f64::NEG_INFINITY, f64::max);
    let x0 = (min_x.floor() as i64).clamp(0, i64::from(width) - 1) as u32;
    let x1 = ((max_x.ceil() as i64) - 1).clamp(0, i64::from(width) - 1) as u32;
    let y0 = (min_y.floor() as i64).clamp(0, i64::from(height) - 1) as u32;
    let y1 = ((max_y.ceil() as i64) - 1).clamp(0, i64::from(height) - 1) as u32;
    if max_x <= 0.0
        || max_y <= 0.0
        || min_x >= f64::from(width)
        || min_y >= f64::from(height)
        || x1 < x0
        || y1 < y0
    {
        return Ok(None);
    }
    let work = u64::from(x1 - x0 + 1) * u64::from(y1 - y0 + 1);
    *raster_work = raster_work
        .checked_add(work)
        .ok_or_else(|| "render work limit overflow".to_string())?;
    if *raster_work > MAX_RASTER_WORK {
        return Err(RASTER_WORK_REFUSAL.to_string());
    }
    let lod = match &target {
        RasterTarget::Color { uvs, texture, mips, .. } if !mips.is_empty() => texture_lod(vertices, *uvs, texture),
        _ => 0.0,
    };
    for y in y0..=y1 {
        for x in x0..=x1 {
            let point = [f64::from(x) + 0.5, f64::from(y) + 0.5];
            let e0 = edge(vertices[1], vertices[2], point);
            let e1 = edge(vertices[2], vertices[0], point);
            let e2 = edge(vertices[0], vertices[1], point);
            let sign = if area > 0.0 { 1.0 } else { -1.0 };
            if !edge_inside(e0, vertices[1], vertices[2], sign)
                || !edge_inside(e1, vertices[2], vertices[0], sign)
                || !edge_inside(e2, vertices[0], vertices[1], sign)
            {
                continue;
            }
            let weights = [e0 / area, e1 / area, e2 / area];
            let pixel = y as usize * width as usize + x as usize;
            match &mut target {
                RasterTarget::Color {
                    uvs,
                    texture,
                    mips,
                    material,
                    canvas,
                    coverage,
                    stencil,
                } => {
                    if let Some(coverage) = coverage.as_deref_mut() {
                        coverage[pixel] = 1;
                    }
                    if (*stencil).is_some_and(|stencil| stencil[pixel] != 1) {
                        continue;
                    }
                    let uv = interpolate_uv(*uvs, weights);
                    let source = sample_lod(texture, mips, uv[0], uv[1], lod);
                    let src = material_color(source, *material);
                    let offset = pixel * 4;
                    let destination = [
                        canvas[offset],
                        canvas[offset + 1],
                        canvas[offset + 2],
                        canvas[offset + 3],
                    ];
                    canvas[offset..offset + 4].copy_from_slice(&blend_material(
                        destination,
                        src,
                        *material,
                    ));
                }
                RasterTarget::Stencil {
                    stencil,
                    value,
                    alpha,
                } => {
                    if let Some(alpha) = alpha {
                        let uv = interpolate_uv(alpha.uvs, weights);
                        if sample(alpha.texture, uv[0], uv[1])[3] <= alpha.threshold {
                            continue;
                        }
                    }
                    stencil[pixel] = *value;
                }
                RasterTarget::Layer {
                    uvs,
                    source,
                    source_width,
                    source_height,
                    canvas,
                    coverage,
                } => {
                    if let Some(coverage) = coverage.as_deref_mut() {
                        coverage[pixel] = 1;
                    }
                    let uv = interpolate_uv(*uvs, weights);
                    let source = sample_layer(source, *source_width, *source_height, uv[0], uv[1]);
                    let offset = pixel * 4;
                    let destination = [
                        canvas[offset],
                        canvas[offset + 1],
                        canvas[offset + 2],
                        canvas[offset + 3],
                    ];
                    canvas[offset..offset + 4].copy_from_slice(&blend_pixel(
                        destination,
                        source,
                        BlendMode::Normal,
                    ));
                }
            }
        }
    }
    Ok(Some([x0, y0, x1, y1]))
}

fn blend_material(destination: [f32; 4], source: [f32; 4], material: Material) -> [f32; 4] {
    if !material.clip_to_destination {
        return blend_pixel(destination, source, material.blend);
    }
    if destination[3] == 0.0 || source[3] == 0.0 {
        return destination;
    }
    let straight = |pixel: [f32; 4]| [pixel[0] / pixel[3], pixel[1] / pixel[3], pixel[2] / pixel[3], 1.0];
    let color = blend_pixel(straight(destination), straight(source), material.blend);
    let mut result = destination;
    for channel in 0..3 {
        result[channel] = color[channel] * source[3] * destination[3]
            + destination[channel] * (1.0 - source[3]);
    }
    result
}

fn blend_pixel(destination: [f32; 4], source: [f32; 4], mode: BlendMode) -> [f32; 4] {
    if let Some(mode) = mode.formula() { return mode.blend(destination, source); }
    let (rgb, alpha): ([f32; 3], f32) = match mode {
        BlendMode::Cubism53(_) | BlendMode::LinearBurn | BlendMode::LinearLight | BlendMode::Hue | BlendMode::Color => {
            unreachable!()
        }
        BlendMode::Normal => (
            std::array::from_fn(|i| source[i] + destination[i] * (1.0 - source[3])),
            source[3] + destination[3] * (1.0 - source[3]),
        ),
        BlendMode::LinearDodge => (
            std::array::from_fn(|i| source[i] + destination[i] * (1.0 - source[i])),
            source[3] + destination[3] * (1.0 - source[3]),
        ),
        BlendMode::AddGlow => (
            std::array::from_fn(|i| source[i] + destination[i]),
            source[3] + destination[3] * (1.0 - source[3]),
        ),
        BlendMode::Inverse => (
            std::array::from_fn(|i| {
                source[i] * (1.0 - destination[i]) + destination[i] * (1.0 - source[3])
            }),
            source[3] * (1.0 - destination[3]) + destination[3] * (1.0 - source[3]),
        ),
        BlendMode::DestinationIn => (
            std::array::from_fn(|i| destination[i] * source[3]),
            destination[3] * source[3],
        ),
        BlendMode::ClipToLower => (
            std::array::from_fn(|i| {
                source[i] * destination[3] + destination[i] * (1.0 - source[3])
            }),
            destination[3],
        ),
        BlendMode::SliceFromLower => (
            std::array::from_fn(|i| destination[i] * (1.0 - source[3])),
            destination[3] * (1.0 - source[3]),
        ),
        BlendMode::Multiply => (
            std::array::from_fn(|i| {
                source[i] * destination[i] + destination[i] * (1.0 - source[3])
            }),
            source[3] * destination[3] + destination[3] * (1.0 - source[3]),
        ),
        BlendMode::Screen => (
            std::array::from_fn(|i| source[i] + destination[i] * (1.0 - source[i])),
            source[3] + destination[3] * (1.0 - source[3]),
        ),
        BlendMode::Lighten => (
            std::array::from_fn(|i| source[i].max(destination[i])),
            source[3].max(destination[3]),
        ),
        BlendMode::ColorDodge => (
            std::array::from_fn(|i| source[i] * destination[i] + destination[i]),
            source[3] * destination[3] + destination[3],
        ),
        BlendMode::Exclusion => (
            std::array::from_fn(|i| {
                source[i] * (1.0 - destination[i]) + destination[i] * (1.0 - source[i])
            }),
            source[3] + destination[3],
        ),
        BlendMode::Subtract => (
            std::array::from_fn(|i| destination[i] - source[i] * (1.0 - destination[i])),
            source[3] * (1.0 - destination[3]) + destination[3],
        ),
        // The active macOS fallback selects legacy blending. These advanced
        // modes therefore use the D renderer's default Normal blend factors.
        BlendMode::Overlay
        | BlendMode::Darken
        | BlendMode::ColorBurn
        | BlendMode::HardLight
        | BlendMode::SoftLight
        | BlendMode::Difference => (
            std::array::from_fn(|i| source[i] + destination[i] * (1.0 - source[3])),
            source[3] + destination[3] * (1.0 - source[3]),
        ),
    };
    [
        rgb[0].clamp(0.0, 1.0),
        rgb[1].clamp(0.0, 1.0),
        rgb[2].clamp(0.0, 1.0),
        alpha.clamp(0.0, 1.0),
    ]
}

fn interpolate_uv(uvs: [[f64; 2]; 3], weights: [f64; 3]) -> [f64; 2] {
    [
        weights[0] * uvs[0][0] + weights[1] * uvs[1][0] + weights[2] * uvs[2][0],
        weights[0] * uvs[0][1] + weights[1] * uvs[1][1] + weights[2] * uvs[2][1],
    ]
}

fn edge_inside(value: f64, a: [f64; 2], b: [f64; 2], sign: f64) -> bool {
    let normalized = value * sign;
    normalized > 0.0 || (normalized == 0.0 && is_top_left(a, b, sign))
}

fn is_top_left(a: [f64; 2], b: [f64; 2], sign: f64) -> bool {
    let (a, b) = if sign > 0.0 { (a, b) } else { (b, a) };
    let dx = b[0] - a[0];
    let dy = b[1] - a[1];
    dy < 0.0 || (dy == 0.0 && dx > 0.0)
}

fn sample(texture: &CpuTexture, u: f64, v: f64) -> [f32; 4] {
    if !u.is_finite() || !v.is_finite() || !(-1.0..=2.0).contains(&u) || !(-1.0..=2.0).contains(&v)
    {
        return [0.0; 4];
    }
    let x = u * f64::from(texture.width) - 0.5;
    let y = v * f64::from(texture.height) - 0.5;
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let x1 = x0 + 1;
    let y1 = y0 + 1;
    let tx = (x - x.floor()) as f32;
    let ty = (y - y.floor()) as f32;
    let p00 = texel(texture, x0, y0);
    let p10 = texel(texture, x1, y0);
    let p01 = texel(texture, x0, y1);
    let p11 = texel(texture, x1, y1);
    std::array::from_fn(|component| {
        let top = p00[component] * (1.0 - tx) + p10[component] * tx;
        let bottom = p01[component] * (1.0 - tx) + p11[component] * tx;
        top * (1.0 - ty) + bottom * ty
    })
}

/// The level of detail of a triangle: log2 of the texels one pixel step covers along the longer screen axis. The UV
/// map of a triangle is affine, so one value holds for all of its pixels.
fn texture_lod(vertices: [[f64; 2]; 3], uvs: [[f64; 2]; 3], texture: &CpuTexture) -> f64 {
    let [p0, p1, p2] = vertices;
    let det = (p1[0] - p0[0]) * (p2[1] - p0[1]) - (p2[0] - p0[0]) * (p1[1] - p0[1]);
    if det.abs() <= f64::EPSILON {
        return 0.0;
    }
    let size = [f64::from(texture.width), f64::from(texture.height)];
    let gradient = |axis: usize| {
        let (a1, a2) = ((uvs[1][axis] - uvs[0][axis]) * size[axis], (uvs[2][axis] - uvs[0][axis]) * size[axis]);
        let dx = (a1 * (p2[1] - p0[1]) - a2 * (p1[1] - p0[1])) / det;
        let dy = (a2 * (p1[0] - p0[0]) - a1 * (p2[0] - p0[0])) / det;
        [dx, dy]
    };
    let (u, v) = (gradient(0), gradient(1));
    let rho = (u[0] * u[0] + v[0] * v[0]).max(u[1] * u[1] + v[1] * v[1]).sqrt();
    if rho.is_finite() && rho > 1.0 {
        rho.log2()
    } else {
        0.0
    }
}

/// `GL_LINEAR_MIPMAP_LINEAR`: bilinear samples of the two levels around `lod`, mixed linearly. Level 0 is `base`.
fn sample_lod(base: &CpuTexture, mips: &[CpuTexture], u: f64, v: f64, lod: f64) -> [f32; 4] {
    if mips.is_empty() || lod <= 0.0 {
        return sample(base, u, v);
    }
    let lod = lod.min(mips.len() as f64);
    let level = |index: usize| if index == 0 { base } else { &mips[index - 1] };
    let lower = lod.floor() as usize;
    let near = sample(level(lower), u, v);
    if lower >= mips.len() {
        return near;
    }
    let far = sample(level(lower + 1), u, v);
    let t = (lod - lod.floor()) as f32;
    std::array::from_fn(|component| near[component] * (1.0 - t) + far[component] * t)
}

/// Each level halves the one above (rounded down, at least 1) with a box filter over the premultiplied texels, down
/// to 1x1.
fn mip_chain(base: &CpuTexture) -> Vec<CpuTexture> {
    let mut chain: Vec<CpuTexture> = Vec::new();
    loop {
        let above = chain.last().unwrap_or(base);
        if above.width == 1 && above.height == 1 {
            return chain;
        }
        let (width, height) = ((above.width / 2).max(1), (above.height / 2).max(1));
        let mut rgba = Vec::with_capacity(width as usize * height as usize * 4);
        for y in 0..height {
            for x in 0..width {
                let xs = [2 * x, (2 * x + 1).min(above.width - 1)];
                let ys = [2 * y, (2 * y + 1).min(above.height - 1)];
                for component in 0..4 {
                    let sum: u32 = ys
                        .iter()
                        .flat_map(|&sy| xs.iter().map(move |&sx| (sy * above.width + sx) as usize * 4 + component))
                        .map(|offset| u32::from(above.rgba[offset]))
                        .sum();
                    rgba.push(((sum + 2) / 4) as u8);
                }
            }
        }
        chain.push(CpuTexture { width, height, rgba });
    }
}

fn sample_layer(layer: &[f32], width: u32, height: u32, u: f64, v: f64) -> [f32; 4] {
    if !u.is_finite() || !v.is_finite() || !(-1.0..=2.0).contains(&u) || !(-1.0..=2.0).contains(&v)
    {
        return [0.0; 4];
    }
    let x = u * f64::from(width) - 0.5;
    let y = v * f64::from(height) - 0.5;
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let tx = (x - x.floor()) as f32;
    let ty = (y - y.floor()) as f32;
    let texel = |x: i64, y: i64| {
        if x < 0 || y < 0 || x >= i64::from(width) || y >= i64::from(height) {
            return [0.0; 4];
        }
        let offset = (y as usize * width as usize + x as usize) * 4;
        [
            layer[offset],
            layer[offset + 1],
            layer[offset + 2],
            layer[offset + 3],
        ]
    };
    let p00 = texel(x0, y0);
    let p10 = texel(x0 + 1, y0);
    let p01 = texel(x0, y0 + 1);
    let p11 = texel(x0 + 1, y0 + 1);
    std::array::from_fn(|component| {
        let top = p00[component] * (1.0 - tx) + p10[component] * tx;
        let bottom = p01[component] * (1.0 - tx) + p11[component] * tx;
        top * (1.0 - ty) + bottom * ty
    })
}

fn texel(texture: &CpuTexture, x: i64, y: i64) -> [f32; 4] {
    if x < 0 || y < 0 || x >= i64::from(texture.width) || y >= i64::from(texture.height) {
        return [0.0; 4];
    }
    let offset = (y as usize * texture.width as usize + x as usize) * 4;
    std::array::from_fn(|component| f32::from(texture.rgba[offset + component]) / 255.0)
}

fn flat_pairs(value: Option<&Value>, path: &str) -> Result<Vec<[f64; 2]>, String> {
    let values = value
        .and_then(Value::as_array)
        .ok_or_else(|| format!("{path} must be a flattened numeric array"))?;
    if values.len() % 2 != 0 {
        return Err(format!("{path} must contain coordinate pairs"));
    }
    values
        .chunks_exact(2)
        .map(|pair| {
            Ok([
                finite_number(&pair[0], path)?,
                finite_number(&pair[1], path)?,
            ])
        })
        .collect()
}

fn vector<const N: usize>(
    value: Option<&Value>,
    min_len: usize,
    fallback: [f64; N],
    path: &str,
) -> Result<[f64; N], String> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    let values = value
        .as_array()
        .ok_or_else(|| format!("{path} must be an array"))?;
    if values.len() < min_len || values.len() > N {
        return Err(format!("{path} has an invalid component count"));
    }
    let mut result = fallback;
    for (index, component) in values.iter().enumerate() {
        result[index] = finite_number(component, path)?;
    }
    Ok(result)
}

fn optional_finite(value: Option<&Value>, fallback: f64, path: &str) -> Result<f64, String> {
    match value {
        Some(value) => finite_number(value, path),
        None => Ok(fallback),
    }
}

fn finite_number(value: &Value, path: &str) -> Result<f64, String> {
    let number = value
        .as_f64()
        .ok_or_else(|| format!("{path} must be a number"))?;
    if !number.is_finite() {
        return Err(format!("{path} must be finite"));
    }
    Ok(number)
}

fn project(matrix: Mat4, point: [f64; 2]) -> Result<[f64; 2], String> {
    let world = bone_deform::transform_point(matrix, [point[0], point[1], 0.0])?;
    Ok([world[0], world[1]])
}

fn edge(a: [f64; 2], b: [f64; 2], p: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (p[1] - a[1]) - (b[1] - a[1]) * (p[0] - a[0])
}

#[cfg(test)]
mod tests {
    #[test]
    fn overlapping_triangles_skip_shared_edges_and_find_folds() {
        let quad = [[0.0, 0.0], [0.0, 2.0], [2.0, 0.0], [2.0, 2.0], [1.0, 1.0], [3.0, 3.0]];
        assert!(super::overlapping_triangles(&quad, &[0, 1, 2, 2, 1, 3]).is_empty(), "a shared edge does not overlap");
        assert!(super::overlapping_triangles(&quad, &[0, 1, 2, 3, 1, 5]).is_empty(), "a shared corner does not overlap");
        assert_eq!(super::overlapping_triangles(&quad, &[0, 1, 2, 2, 1, 3, 4, 3, 2]), vec![[1, 2]]);
        assert_eq!(super::overlapping_triangles(&quad, &[0, 1, 2, 0, 1, 3]), vec![[0, 1]], "folded over a shared edge");
        assert!(super::overlapping_triangles(&quad, &[0, 4, 3, 0, 1, 2]).is_empty(), "a degenerate triangle is skipped");
        assert!(super::overlapping_triangles(&quad, &[0, 1, 2, 0, 1, 9]).is_empty(), "an index past the end is skipped");
    }

    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn red_texture(color: [u8; 4]) -> TextureBlob {
        let image = RgbaImage::from_pixel(4, 4, image::Rgba(color));
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut bytes, ImageFormat::Png)
            .unwrap();
        TextureBlob {
            tag: 0,
            data: bytes.into_inner(),
        }
    }

    fn part(indices: Value) -> Value {
        json!({
            "uuid": 2, "type": "Part", "enabled": true, "zsort": 0,
            "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]},
            "textures": [0u64, u32::MAX, u32::MAX],
            "mesh": {
                "verts": [0,0, 0,8, 8,0, 8,8],
                "uvs": [0,0, 0,1, 1,0, 1,1],
                "indices": indices,
                "origin": [0,0]
            }
        })
    }

    fn scene(part: Value) -> Value {
        scene_with(vec![part])
    }

    fn scene_with(children: Vec<Value>) -> Value {
        json!({
            "nodes": {
                "uuid": 1, "type": "Node", "enabled": true, "zsort": 0,
                "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]},
                "children": children
            }
        })
    }

    fn render_part(id: u32, texture: Value, zsort: i32) -> Value {
        let mut part = part(json!([0, 1, 2, 2, 1, 3]));
        part["uuid"] = json!(id);
        part["textures"] = json!([texture, u32::MAX, u32::MAX]);
        part["zsort"] = json!(zsort);
        part
    }

    fn composite(id: u32, children: Vec<Value>, opacity: f64) -> Value {
        json!({
            "uuid": id, "type": "Composite", "enabled": true, "zsort": 0,
            "blend_mode": "Normal", "opacity": opacity, "tint": [1.0, 1.0, 1.0],
            "screenTint": [0.0, 0.0, 0.0], "mask_threshold": 0.5,
            "propagate_meshgroup": false,
            "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]},
            "children": children
        })
    }

    fn mask(id: u32, vertices: Value, indices: Value) -> Value {
        json!({
            "uuid": id, "type": "Mask", "enabled": true,
            "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]},
            "mesh": {"verts": vertices, "indices": indices, "origin": [0,0]}
        })
    }

    fn pixel(png: &[u8], x: u32, y: u32) -> [u8; 4] {
        let image = image::load_from_memory_with_format(png, ImageFormat::Png)
            .unwrap()
            .into_rgba8();
        image.get_pixel(x, y).0
    }

    fn has_visible_pixels(png: &[u8]) -> bool {
        image::load_from_memory_with_format(png, ImageFormat::Png)
            .unwrap()
            .into_rgba8()
            .pixels()
            .any(|pixel| pixel.0[3] != 0)
    }

    fn assert_d_composite_alpha_patch<const W: usize, const H: usize>(
        png: &[u8],
        x_start: u32,
        y_start: u32,
        expected: [[u8; W]; H],
    ) {
        for y in 0..16 {
            for x in 0..16 {
                let expected_alpha = if (x_start..x_start + W as u32).contains(&x)
                    && (y_start..y_start + H as u32).contains(&y)
                {
                    expected[(y - y_start) as usize][(x - x_start) as usize]
                } else {
                    0
                };
                let actual = pixel(png, x, y);
                assert!(
                    actual[3].abs_diff(expected_alpha) <= 2,
                    "alpha at ({x}, {y}): {} != {expected_alpha}",
                    actual[3]
                );
                let premultiplied_red =
                    ((u16::from(actual[0]) * u16::from(actual[3]) + 127) / 255) as u8;
                assert!(
                    premultiplied_red.abs_diff(expected_alpha) <= 2,
                    "premultiplied red at ({x}, {y})"
                );
                assert_eq!(actual[1..3], [0, 0], "green/blue at ({x}, {y})");
            }
        }
    }

    fn assert_d_composite_pixel(png: &[u8], x: u32, y: u32, expected_alpha: u8) {
        let actual = pixel(png, x, y);
        assert!(
            actual[3].abs_diff(expected_alpha) <= 2,
            "alpha at ({x}, {y}): {} != {expected_alpha}",
            actual[3]
        );
        let premultiplied_red = ((u16::from(actual[0]) * u16::from(actual[3]) + 127) / 255) as u8;
        assert!(
            premultiplied_red.abs_diff(expected_alpha) <= 2,
            "premultiplied red at ({x}, {y})"
        );
        assert_eq!(actual[1..3], [0, 0], "green/blue at ({x}, {y})");
    }

    fn premultiplied_pixel(png: &[u8], x: u32, y: u32) -> [f32; 4] {
        let pixel = pixel(png, x, y).map(|value| f32::from(value) / 255.0);
        [
            pixel[0] * pixel[3],
            pixel[1] * pixel[3],
            pixel[2] * pixel[3],
            pixel[3],
        ]
    }

    fn assert_premultiplied_pixel(png: &[u8], x: u32, y: u32, expected: [f32; 4]) {
        let actual = premultiplied_pixel(png, x, y);
        for (actual, expected) in actual.into_iter().zip(expected) {
            assert!(
                (actual - expected).abs() <= 1.0 / 255.0,
                "{actual} != {expected}"
            );
        }
    }

    #[test]
    fn mip_levels_halve_with_a_box_filter_down_to_one_texel() {
        let base = CpuTexture { width: 4, height: 2, rgba: [0u8, 40, 80, 120].repeat(8) };
        let chain = mip_chain(&base);
        let sizes: Vec<_> = chain.iter().map(|level| (level.width, level.height)).collect();
        assert_eq!(sizes, [(2, 1), (1, 1)]);
        assert_eq!(chain[1].rgba, [0, 40, 80, 120]);
        let odd = CpuTexture { width: 3, height: 1, rgba: vec![0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 255] };
        assert_eq!(mip_chain(&odd)[0].rgba, [128, 128, 128, 255], "the last odd column is dropped, as in GL");
    }

    #[test]
    fn a_mipmapped_part_minifies_through_its_mip_chain() {
        // One white texel in each 4x4 block of an 8x8 texture. A 2x2 picture of the 8x8 Part steps 4 texels a pixel
        // (level 2, the 4x4 average); the base level samples between four black texels.
        let image = RgbaImage::from_fn(8, 8, |x, y| {
            if x % 4 == 0 && y % 4 == 0 { image::Rgba([255, 255, 255, 255]) } else { image::Rgba([0, 0, 0, 255]) }
        });
        let mut bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image).write_to(&mut bytes, ImageFormat::Png).unwrap();
        let textures = [TextureBlob { tag: 0, data: bytes.into_inner() }];
        let plain = part(json!([0, 1, 2, 2, 1, 3]));
        let mut flagged = plain.clone();
        flagged["textureMipmaps"] = json!(true);
        let base = render_png(&scene(plain.clone()), &textures, 2, 2, [0.0, 0.0, 8.0, 8.0]).unwrap();
        let mipmapped = render_png(&scene(flagged.clone()), &textures, 2, 2, [0.0, 0.0, 8.0, 8.0]).unwrap();
        for (x, y) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
            assert_eq!(pixel(&base, x, y)[0], 0);
            assert_eq!(pixel(&mipmapped, x, y)[0], 16, "255 / 16, rounded");
        }
        // Magnified, the flag changes nothing.
        let near = render_png(&scene(plain), &textures, 16, 16, [0.0, 0.0, 8.0, 8.0]).unwrap();
        assert_eq!(render_png(&scene(flagged), &textures, 16, 16, [0.0, 0.0, 8.0, 8.0]).unwrap(), near);
    }

    #[test]
    fn samples_transparent_clamp_border_without_overflowing_far_uvs() {
        let texture = CpuTexture {
            width: 1,
            height: 1,
            rgba: vec![255, 0, 0, 255],
        };
        assert_eq!(sample(&texture, 0.0, 0.5), [0.5, 0.0, 0.0, 0.5]);
        assert_eq!(sample(&texture, f64::MAX, 0.5), [0.0; 4]);
    }

    #[test]
    fn bounds_follow_parent_and_local_transforms() {
        let mut model = scene(part(json!([0, 1, 2, 2, 1, 3])));
        model["nodes"]["transform"]["trans"] = json!([100, 20, 0]);
        model["nodes"]["children"][0]["transform"]["scale"] = json!([2, 3]);
        assert_eq!(scene_bounds(&model).unwrap(), [98.4, 17.6, 117.6, 46.4]);
    }

    #[test]
    fn bounds_follow_translated_composite_output_and_render_within_them() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([-4, -4, -4, 4, 4, -4, 4, 4]);
        let mut group = composite(10, vec![child], 1.0);
        group["transform"]["trans"] = json!([10, 20, 0]);
        let mut model = scene(group);
        model["nodes"]["transform"]["trans"] = json!([30, 40, 0]);

        let ordinary_bounds = scene_bounds(&scene(part(json!([0, 1, 2, 2, 1, 3])))).unwrap();
        assert_eq!(ordinary_bounds, [-0.8, -0.8, 8.8, 8.8]);

        let base = scene_bounds(&model).unwrap();
        let deformations = BTreeMap::from([(10, vec![[100.0, 0.0]; 4])]);
        let bounds = scene_bounds_with(&model, &deformations).unwrap();
        assert!(
            (bounds[0] - base[0] - 100.0).abs() < 1e-9,
            "{bounds:?} {base:?}"
        );
        assert!(
            (bounds[2] - base[2] - 100.0).abs() < 1e-9,
            "{bounds:?} {base:?}"
        );
        assert_eq!(bounds[1], base[1]);
        assert_eq!(bounds[3], base[3]);

        let png = render_png_with(
            &model,
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            32,
            32,
            bounds,
        )
        .unwrap();
        assert!(has_visible_pixels(&png));
    }

    #[test]
    fn nested_composite_deformation_moves_bounds_and_renders_within_them() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([-4, -4, -4, 4, 4, -4, 4, 4]);
        let inner = composite(11, vec![child], 1.0);
        let outer = composite(10, vec![inner], 1.0);
        let mut model = scene(outer);
        model["nodes"]["transform"]["trans"] = json!([30, 40, 0]);

        let base = scene_bounds(&model).unwrap();
        let deformations = BTreeMap::from([(11, vec![[100.0, 0.0]; 4])]);
        let bounds = scene_bounds_with(&model, &deformations).unwrap();
        assert!(
            (bounds[0] - base[0] - 100.0).abs() < 1e-9,
            "{bounds:?} {base:?}"
        );
        assert!(
            (bounds[2] - base[2] - 100.0).abs() < 1e-9,
            "{bounds:?} {base:?}"
        );

        let png = render_png_with(
            &model,
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            32,
            32,
            bounds,
        )
        .unwrap();
        assert!(has_visible_pixels(&png));
    }

    #[test]
    fn hidden_composite_mask_scope_does_not_expand_bounds() {
        let baseline = scene(render_part(2, json!(0), 0));
        let mut target = render_part(2, json!(0), 0);
        target["masks"] = json!([{"source": 3, "mode": "Mask"}]);
        let mut hidden_child = render_part(4, json!(1), 0);
        hidden_child["mesh"]["verts"] = json!([500, 500, 500, 508, 508, 500, 508, 508]);
        let mut hidden_mask = composite(3, vec![hidden_child], 1.0);
        hidden_mask["enabled"] = json!(false);

        assert_eq!(
            scene_bounds(&scene_with(vec![target, hidden_mask])).unwrap(),
            scene_bounds(&baseline).unwrap()
        );
    }

    #[test]
    fn renders_premultiplied_texture_once_across_triangle_seam() {
        let png = render_png(
            &scene(part(json!([0, 1, 2, 2, 1, 3]))),
            &[red_texture([128, 0, 0, 128])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 3, 3), [255, 0, 0, 128]);
        assert_eq!(pixel(&png, 4, 4), [255, 0, 0, 128]);
    }

    #[test]
    fn solo_draws_only_the_chosen_parts_with_their_composites_and_masks() {
        let solo = |root: &Value, textures: &[TextureBlob], only: &[u64]| {
            let geometry = crate::scene_deform::evaluate(root, &BTreeMap::new()).unwrap();
            let only: HashSet<u64> = only.iter().copied().collect();
            render_png_solo(root, &geometry, textures, 8, 8, [0.0, 0.0, 8.0, 8.0], [1.0; 3], &only, None).unwrap()
        };
        let textures = [red_texture([255, 0, 0, 255]), red_texture([0, 0, 255, 255])];
        // A lower zsort draws on top. Red on top of blue: solo blue shows blue, solo red red, solo nothing nothing.
        let root = scene_with(vec![render_part(2, json!(0), 0), render_part(3, json!(1), 1)]);
        assert_eq!(pixel(&render_png(&root, &textures, 8, 8, [0.0, 0.0, 8.0, 8.0]).unwrap(), 3, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&solo(&root, &textures, &[3]), 3, 3), [0, 0, 255, 255]);
        assert_eq!(pixel(&solo(&root, &textures, &[2]), 3, 3), [255, 0, 0, 255]);
        assert!(!has_visible_pixels(&solo(&root, &textures, &[])));
        // A Part inside a Composite draws through the Composite's opacity, without its sibling.
        let group = composite(10, vec![render_part(2, json!(0), 1), render_part(3, json!(1), 0)], 0.5);
        let root = scene_with(vec![group]);
        assert_premultiplied_pixel(&solo(&root, &textures, &[3]), 3, 3, [0.0, 0.0, 0.5, 0.5]);
        assert!(!has_visible_pixels(&solo(&root, &textures, &[4])));
        // A mask source outside the solo set still clips the Part, and is not drawn itself.
        let mut target = render_part(2, json!(0), 0);
        target["masks"] = json!([{"source": 5, "mode": "Mask"}]);
        let left = mask(5, json!([0, 0, 0, 8, 4, 0, 4, 8]), json!([0, 1, 2, 2, 1, 3]));
        let root = scene_with(vec![target.clone(), left]);
        let png = solo(&root, &textures, &[2]);
        assert_eq!((pixel(&png, 1, 3), pixel(&png, 6, 3)), ([255, 0, 0, 255], [0, 0, 0, 0]));
        // The same with a blue Part over the left half as the mask source.
        let mut left = render_part(5, json!(1), -1);
        left["mesh"]["verts"] = json!([0, 0, 0, 8, 4, 0, 4, 8]);
        let root = scene_with(vec![target, left]);
        assert_eq!(pixel(&render_png(&root, &textures, 8, 8, [0.0, 0.0, 8.0, 8.0]).unwrap(), 1, 3), [0, 0, 255, 255]);
        let png = solo(&root, &textures, &[2]);
        assert_eq!((pixel(&png, 1, 3), pixel(&png, 6, 3)), ([255, 0, 0, 255], [0, 0, 0, 0]));
        // A Composite mask source draws whole, its children outside the solo set too.
        let mut target = render_part(2, json!(0), 0);
        target["masks"] = json!([{"source": 6, "mode": "Mask"}]);
        let mut left = render_part(7, json!(1), 0);
        left["mesh"]["verts"] = json!([0, 0, 0, 8, 4, 0, 4, 8]);
        let mut source = composite(6, vec![left], 1.0);
        source["enabled"] = json!(false);
        // The blue Part draws after the masked one and stays out of the solo picture.
        let png = solo(&scene_with(vec![target, source, render_part(8, json!(1), -1)]), &textures, &[2]);
        assert_eq!((pixel(&png, 1, 3), pixel(&png, 6, 3)), ([255, 0, 0, 255], [0, 0, 0, 0]));
    }

    #[test]
    fn solo_with_a_cover_draws_the_chosen_parts_over_the_covered_picture() {
        let textures = [red_texture([255, 0, 0, 255]), red_texture([0, 0, 255, 255])];
        // Blue on the left half, a Multiply red Part over all of it.
        let mut left = render_part(3, json!(1), 1);
        left["mesh"]["verts"] = json!([0, 0, 0, 8, 4, 0, 4, 8]);
        let mut multiply = render_part(2, json!(0), 0);
        multiply["blend_mode"] = json!("Multiply");
        let root = scene_with(vec![left.clone(), multiply.clone()]);
        let geometry = crate::scene_deform::evaluate(&root, &BTreeMap::new()).unwrap();
        let only = HashSet::from([2]);
        let render = |cover| {
            render_png_solo(&root, &geometry, &textures, 8, 8, [0.0, 0.0, 8.0, 8.0], [1.0; 3], &only, cover).unwrap()
        };
        // Without a cover a Multiply Part has nothing below it to darken, so it leaves nothing.
        assert!(!has_visible_pixels(&render(None)));
        // White at half opacity over blue and over nothing, then red multiplied over that.
        let png = render(Some([255, 255, 255, 128]));
        assert_eq!(pixel(&png, 1, 3), [128, 0, 0, 255]);
        assert_eq!(pixel(&png, 6, 3), [255, 0, 0, 128]);
        // Each item is drawn once, so a clear cover leaves a translucent solo Part as the normal picture has it.
        multiply["opacity"] = json!(0.5);
        let root = scene_with(vec![left, multiply]);
        let geometry = crate::scene_deform::evaluate(&root, &BTreeMap::new()).unwrap();
        let clear = Some([0; 4]);
        let png = render_png_solo(&root, &geometry, &textures, 8, 8, [0.0, 0.0, 8.0, 8.0], [1.0; 3], &only, clear);
        let normal = render_png(&root, &textures, 8, 8, [0.0, 0.0, 8.0, 8.0]).unwrap();
        assert_eq!(pixel(&normal, 1, 3), [0, 0, 128, 255]);
        assert_eq!(png.unwrap(), normal);
        // A half-opacity Composite holding a solo red Part on the left and a blue one on the right: each pass draws
        // it with its own child, the blue one under a white cover at alpha 128 and the red one over it.
        let mut red = render_part(2, json!(0), 0);
        red["mesh"]["verts"] = json!([0, 0, 0, 8, 4, 0, 4, 8]);
        let mut blue = render_part(3, json!(1), 0);
        blue["mesh"]["verts"] = json!([4, 0, 4, 8, 8, 0, 8, 8]);
        let root = scene_with(vec![composite(10, vec![red, blue], 0.5)]);
        let geometry = crate::scene_deform::evaluate(&root, &BTreeMap::new()).unwrap();
        let cover = Some([255, 255, 255, 128]);
        let png = render_png_solo(&root, &geometry, &textures, 8, 8, [0.0, 0.0, 8.0, 8.0], [1.0; 3], &only, cover);
        let png = png.unwrap();
        let white = 128.0 / 255.0;
        assert_premultiplied_pixel(&png, 1, 3, [0.5 + white * 0.5, white * 0.5, white * 0.5, 0.5 + white * 0.5]);
        assert_premultiplied_pixel(&png, 6, 3, [white, white, white + 0.5 * (1.0 - white), white + 0.5 * (1.0 - white)]);
    }

    #[test]
    fn composites_render_children_as_one_z_sorted_opacity_layer() {
        let group = composite(
            10,
            vec![render_part(2, json!(0), 2), render_part(3, json!(1), -2)],
            0.5,
        );
        let png = render_png(
            &scene(group),
            &[red_texture([255, 0, 0, 255]), red_texture([0, 0, 255, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_premultiplied_pixel(&png, 3, 3, [0.0, 0.0, 0.5, 0.5]);

        let group = composite(
            10,
            vec![render_part(2, json!(0), 2), render_part(3, json!(1), -2)],
            0.5,
        );
        let green = render_part(4, json!(2), 1);
        let root = scene_with(vec![green, group]);
        let png = render_png(
            &root,
            &[
                red_texture([255, 0, 0, 255]),
                red_texture([0, 0, 255, 255]),
                red_texture([0, 255, 0, 255]),
            ],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_premultiplied_pixel(&png, 3, 3, [0.0, 0.5, 0.5, 1.0]);
    }

    #[test]
    fn nested_composite_opacity_multiplies_once_per_scope() {
        let inner = composite(10, vec![render_part(2, json!(0), 0)], 0.5);
        let outer = composite(11, vec![inner], 0.5);
        let png = render_png(
            &scene(outer),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_premultiplied_pixel(&png, 3, 3, [0.25, 0.0, 0.0, 0.25]);
    }

    #[test]
    fn many_small_composites_on_a_large_target_stay_within_the_work_limit() {
        // Two full-target passes per composite would cost 240 * 2 * 512 * 512 pixels, above MAX_RASTER_WORK.
        let groups = (0..240)
            .map(|index| composite(1000 + index, vec![render_part(2000 + index, json!(0), 0)], 1.0))
            .collect();
        let png = render_png(
            &scene_with(groups),
            &[red_texture([255, 0, 0, 255])],
            512,
            512,
            [0.0, 0.0, 512.0, 512.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 3, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 8, 8), [0, 0, 0, 0]);
        assert_eq!(pixel(&png, 300, 300), [0, 0, 0, 0]);
    }

    #[test]
    fn composite_transform_is_not_applied_again_to_its_world_space_layer() {
        let mut group = composite(10, vec![render_part(2, json!(0), 0)], 1.0);
        group["transform"]["trans"] = json!([2, 0, 0]);
        let png = render_png(
            &scene(group),
            &[red_texture([255, 0, 0, 255])],
            12,
            8,
            [0.0, 0.0, 12.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 3, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 1, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn applies_composite_screen_tint_to_premultiplied_albedo() {
        let material = Material {
            clip_to_destination: false,
            blend: BlendMode::Normal,
            tint: [1.0; 3],
            screen_tint: [0.5, 0.0, 0.0],
            opacity: 0.5,
            native_tint: false,
        };
        assert_eq!(
            composite_source([0.25, 0.0, 0.0, 0.5], material),
            [0.21875, 0.0, 0.0, 0.25]
        );
    }

    #[test]
    fn applies_part_screen_tint_and_opacity_to_albedo() {
        let mut part = render_part(2, json!(0), 0);
        part["screenTint"] = json!([0.5, 0.0, 0.0]);
        part["opacity"] = json!(0.5);
        let png = render_png(
            &scene(part),
            &[red_texture([64, 0, 0, 128])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        let color = 64.0 / 255.0;
        let alpha = 128.0 / 255.0;
        let red = (1.0 - (1.0 - color) * (1.0 - 0.5 * alpha)) * 0.5;
        assert_premultiplied_pixel(&png, 3, 3, [red, 0.0, 0.0, alpha * 0.5]);
    }

    #[test]
    fn native_artmesh_tint_multiplies_before_screening_opaque_pixels() {
        let mut part = render_part(2, json!(0), 0);
        part["blend_mode"] = json!("Normal");
        part["nativeColorBlend"] = json!("NORMAL");
        part["nativeAlphaBlend"] = json!("OVER");
        part["tint"] = json!([0.5, 0.25, 0.75]);
        part["screenTint"] = json!([0.4, 0.5, 0.2]);
        let png = render_png(
            &scene(part),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 3, 3), [179, 128, 51, 255]);
    }

    #[test]
    fn native_composite_tint_matches_shader_for_transparent_premultiplied_pixels() {
        let node = json!({
            "blend_mode": "Normal", "nativeColorBlend": "NORMAL", "nativeAlphaBlend": "OVER",
            "tint": [0.5, 0.25, 1.0], "screenTint": [0.5, 0.25, 0.5], "opacity": 0.5
        });
        let material = material(&node, 1, "Composite").unwrap();
        let actual = composite_source([0.2, 0.1, 0.05, 0.25], material);
        for (actual, expected) in actual.into_iter().zip([0.0875, 0.040625, 0.075, 0.125]) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
        assert_eq!(composite_source([0.0; 4], material), [0.0; 4]);
    }

    #[test]
    fn macos_legacy_blend_fallback_matches_d_gl_factors() {
        let destination = [0.2, 0.4, 0.6, 1.0];
        let source = [0.3, 0.2, 0.1, 0.5];
        let multiply = blend_pixel(destination, source, BlendMode::Multiply);
        let screen = blend_pixel(destination, source, BlendMode::Screen);
        let overlay = blend_pixel(destination, [0.75, 0.75, 0.75, 1.0], BlendMode::Overlay);
        for (actual, expected) in multiply.into_iter().zip([0.16, 0.28, 0.36, 1.0]) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
        for (actual, expected) in screen.into_iter().zip([0.44, 0.52, 0.64, 1.0]) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
        for (actual, expected) in overlay.into_iter().zip([0.75, 0.75, 0.75, 1.0]) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }

    #[test]
    fn native_materials_render_without_being_silently_downgraded_in_packets() {
        let node = json!({"blend_mode":"Cubism53", "nativeColorBlend":"MULTIPLY_R2", "nativeAlphaBlend":"ATOP"});
        let parsed = material(&node, 1, "Part").unwrap();
        let actual = blend_pixel([0.1, 0.3, 0.4, 0.5], [0.2, 0.1, 0.05, 0.25], parsed.blend);
        for (value, expected) in actual.into_iter().zip([0.095, 0.255, 0.32, 0.5]) {
            assert!((value - expected).abs() < 1e-6);
        }
        assert!(packet_material(&node, 1, "Part", true).unwrap_err().contains("GPU packets do not support"));
    }

    #[test]
    fn the_four_inx_modes_with_a_formula_blend_as_over_and_stay_out_of_packets() {
        // Half a destination of (0.8, 0.6, 0.4) under a quarter of a source of (0.5, 0.7, 0.2), both premultiplied.
        // Over gives the blended colour 0.125, the source 0.125 and the destination 0.375 of the pixel. The
        // blended colours, by hand: (0.3, 0.3, 0), (0.8, 1, 0), (0.57, 0.73, 0.33) and (0.553, 0.753, 0.253).
        let cases = [
            ("LinearBurn", [0.4, 0.35, 0.175]),
            ("LinearLight", [0.4625, 0.4375, 0.175]),
            ("Hue", [0.43375, 0.40375, 0.21625]),
            ("Color", [0.431625, 0.406625, 0.206625]),
        ];
        for (name, expected) in cases {
            let node = json!({"blend_mode": name});
            let parsed = material(&node, 7, "Part").unwrap();
            assert_eq!(parsed.blend.canonical(), name);
            let actual = blend_pixel([0.4, 0.3, 0.2, 0.5], [0.125, 0.175, 0.05, 0.25], parsed.blend);
            for (value, expected) in actual.into_iter().zip(expected.into_iter().chain([0.625])) {
                assert!((value - expected).abs() < 1e-6, "{name}: {actual:?}");
            }
            assert_eq!(
                packet_material(&node, 7, "Part", true).unwrap_err(),
                format!("Part node 7 blends as {name}, which the CPU renderer draws; GPU packets do not support it")
            );
            assert_eq!(packet_material(&node, 7, "Part", false).unwrap()["blendMode"], name);
        }
    }

    #[test]
    fn an_alpha_blend_draws_the_cubism_formula_of_the_same_name() {
        let destination = [0.4, 0.3, 0.2, 0.5];
        let source = [0.125, 0.175, 0.05, 0.25];
        let blend = |node: Value| blend_pixel(destination, source, material(&node, 7, "Part").unwrap().blend);
        let cases = [
            ("Normal", "NORMAL"), ("LinearDodge", "ADD_R2"), ("AddGlow", "ADD_GLOW"), ("Darken", "DARKEN"),
            ("Multiply", "MULTIPLY_R2"), ("ColorBurn", "COLOR_BURN"), ("LinearBurn", "LINEAR_BURN"),
            ("Lighten", "LIGHTEN"), ("Screen", "SCREEN"), ("ColorDodge", "COLOR_DODGE"), ("Overlay", "OVERLAY"),
            ("SoftLight", "SOFTLIGHT"), ("HardLight", "HARDLIGHT"), ("LinearLight", "LINEAR_LIGHT"),
            ("Hue", "HSL_HUE"), ("Color", "HSL_COLOR"),
        ];
        for (inx, color) in cases {
            let alphas = [("Atop", "ATOP"), ("Out", "OUT"), ("Conjoint", "CONJOINT"), ("Disjoint", "DISJOINT")];
            for (alpha, native) in alphas {
                let expected =
                    blend(json!({"blend_mode": "Cubism53", "nativeColorBlend": color, "nativeAlphaBlend": native}));
                assert_eq!(blend(json!({"blend_mode": inx, "alphaBlend": alpha})), expected, "{inx} {alpha}");
            }
            // Over, set or not, keeps the INX formula.
            let inx_formula = blend(json!({"blend_mode": inx}));
            assert_eq!(blend(json!({"blend_mode": inx, "alphaBlend": "Over"})), inx_formula, "{inx}");
        }
        // Normal with Atop and Out composites as the fixed INX modes that clip to and slice from what lies below.
        let close = |actual: [f32; 4], expected: [f32; 4]| {
            assert!(actual.iter().zip(expected).all(|(a, e)| (a - e).abs() < 1e-6), "{actual:?} != {expected:?}");
        };
        close(blend(json!({"alphaBlend": "Atop"})), blend(json!({"blend_mode": "ClipToLower"})));
        close(blend(json!({"alphaBlend": "Out"})), blend(json!({"blend_mode": "SliceFromLower"})));
        // By hand: the product (0.4, 0.42, 0.08) on the overlap 0.125 and what lies below on 0.375.
        close(blend(json!({"blend_mode": "Multiply", "alphaBlend": "Atop"})), [0.35, 0.2775, 0.16, 0.5]);

        let without =
            ["Difference", "Exclusion", "Subtract", "Inverse", "DestinationIn", "ClipToLower", "SliceFromLower"];
        for mode in without {
            let node = json!({"blend_mode": mode, "alphaBlend": "Disjoint"});
            assert_eq!(
                check_blend(&node, 7, "Part").unwrap_err(),
                format!("Part node 7 blends as {mode}, which has no Cubism alpha blend; alphaBlend must be Over")
            );
            assert!(check_blend(&json!({"blend_mode": mode, "alphaBlend": "Over"}), 7, "Part").is_ok());
        }
        let cubism = json!({
            "blend_mode": "Cubism53", "nativeColorBlend": "NORMAL", "nativeAlphaBlend": "OVER", "alphaBlend": "Atop"
        });
        assert!(check_blend(&cubism, 7, "Part").unwrap_err().contains("blends as Cubism53"));
        for alpha in [json!("atop"), json!("ConjointOver"), json!(1)] {
            assert_eq!(
                check_blend(&json!({"alphaBlend": alpha}), 7, "Part").unwrap_err(),
                "Part node 7.alphaBlend must be Over, Atop, Out, Conjoint or Disjoint"
            );
        }

        let atop = json!({"blend_mode": "Normal", "alphaBlend": "Atop"});
        assert_eq!(
            packet_material(&atop, 7, "Part", true).unwrap_err(),
            "Part node 7 has alphaBlend Atop, which the CPU renderer draws; GPU packets do not support it"
        );
        assert!(packet_material(&atop, 7, "Part", false).is_ok());
        assert_eq!(packet_material(&json!({"alphaBlend": "Over"}), 7, "Part", true).unwrap()["blendMode"], "Normal");

        // A Composite draws its layer with the composition: Out over an empty canvas leaves nothing.
        let drawn = |alpha: &str| {
            let mut group = composite(3, vec![render_part(2, json!(0), 0)], 1.0);
            group["alphaBlend"] = json!(alpha);
            let png = render_png(&scene(group), &[red_texture([255, 0, 0, 255])], 8, 8, [0.0, 0.0, 8.0, 8.0]);
            pixel(&png.unwrap(), 3, 3)
        };
        assert_eq!(drawn("Over"), [255, 0, 0, 255]);
        assert_eq!(drawn("Disjoint"), [255, 0, 0, 255]);
        assert_eq!(drawn("Out"), [0, 0, 0, 0]);
    }

    #[test]
    fn scene_geometry_reads_a_part_whose_blend_the_gpu_packet_refuses() {
        let mut part = render_part(2, json!(0), 0);
        part["blend_mode"] = json!("Cubism53");
        part["nativeColorBlend"] = json!("MULTIPLY_R2");
        part["nativeAlphaBlend"] = json!("ATOP");
        part["opacity"] = json!(0.25);
        let model = scene(part);
        let geometry = crate::scene_deform::evaluate(&model, &BTreeMap::new()).unwrap();
        let textures = [red_texture([128, 0, 0, 128])];
        assert!(render_packet_for_scene(&model, &geometry, &textures)
            .unwrap_err()
            .contains("GPU packets do not support"));
        let read = scene_geometry_for_scene(&model, &geometry, &textures).unwrap();
        assert_eq!(read["parts"].as_array().unwrap().len(), 1);
        assert_eq!(read["parts"][0]["id"], 2);
        assert_eq!(read["parts"][0]["vertices"], json!([[0.0, 0.0], [0.0, 8.0], [8.0, 0.0], [8.0, 8.0]]));
        assert_eq!(read["parts"][0]["opacity"], 0.25, "the evaluated opacity the Part is drawn with");
    }

    #[test]
    fn scene_geometry_reads_composites_too_large_for_gpu_surfaces() {
        let big_part = |id: u32, size: u32| {
            let mut part = render_part(id, json!(0), 0);
            part["mesh"]["verts"] = json!([0, 0, 0, size, size, 0, size, size]);
            part
        };
        let textures = [red_texture([128, 0, 0, 128])];
        let read = |model: &Value| {
            let geometry = crate::scene_deform::evaluate(model, &BTreeMap::new()).unwrap();
            (
                render_packet_for_scene(model, &geometry, &textures).map(|_| ()),
                scene_geometry_for_scene(model, &geometry, &textures).unwrap()["parts"].clone(),
            )
        };
        let corners = |size: f64| json!([[0.0, 0.0], [0.0, size], [size, 0.0], [size, size]]);

        // One surface over the pixel limit of a layer.
        let (packet, parts) = read(&scene(composite(3, vec![big_part(2, 6000)], 1.0)));
        assert_eq!(packet.unwrap_err(), "Composite node 3 source layer exceeds the 16-megapixel limit");
        assert_eq!(
            parts,
            json!([{"id": 2, "indices": [0, 1, 2, 2, 1, 3], "zsort": 0.0, "opacity": 1.0, "vertices": corners(6000.0)}])
        );

        // Two surfaces of 4000 x 4000 pixels at 26 bytes each are over the 640-megabyte packet limit together.
        let pair = scene_with(vec![
            composite(3, vec![big_part(2, 4000)], 1.0),
            composite(5, vec![big_part(4, 4000)], 1.0),
        ]);
        let (packet, parts) = read(&pair);
        assert_eq!(packet.unwrap_err(), "render packet exceeds the 640-megabyte resource limit");
        assert_eq!(parts.as_array().unwrap().len(), 2);
        assert_eq!(parts[0]["vertices"], corners(4000.0));
    }

    #[test]
    fn packet_marks_native_tint_and_premultiplied_textures_without_changing_legacy_shape() {
        let build_packet = |mut part: Value| {
            part["blend_mode"] = json!("Normal");
            let model = scene(part);
            let geometry = crate::scene_deform::evaluate(&model, &BTreeMap::new()).unwrap();
            render_packet_for_scene(&model, &geometry, &[red_texture([128, 0, 0, 128])]).unwrap()
        };

        let mut native = render_part(2, json!(0), 0);
        native["nativeColorBlend"] = json!("NORMAL");
        native["nativeAlphaBlend"] = json!("OVER");
        let packet = build_packet(native);
        assert_eq!(packet["draws"][0]["material"]["nativeTint"], true);
        assert_eq!(packet["textures"][0]["premultiplied"], true);

        let packet = build_packet(render_part(2, json!(0), 0));
        assert!(packet["draws"][0]["material"].get("nativeTint").is_none());
        assert!(packet["textures"][0].get("premultiplied").is_none());
    }

    #[test]
    fn native_tint_marker_pair_requires_both_strings() {
        let node = json!({"blend_mode": "Normal", "nativeColorBlend": "NORMAL"});
        assert!(material(&node, 1, "Part")
            .err().unwrap()
            .contains("nativeAlphaBlend must be a string"));
        assert!(packet_material(&node, 1, "Part", true).is_err());
    }

    #[test]
    fn legacy_blend_modes_match_d_gl_blend_factors() {
        let destination = [0.2, 0.4, 0.6, 0.5];
        let source = [0.3, 0.2, 0.1, 0.5];
        let add_glow = blend_pixel(destination, source, BlendMode::AddGlow);
        let destination_in = blend_pixel(destination, source, BlendMode::DestinationIn);
        let slice = blend_pixel(destination, source, BlendMode::SliceFromLower);
        for (actual, expected) in add_glow.into_iter().zip([0.5, 0.6, 0.7, 0.75]) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
        for (actual, expected) in destination_in.into_iter().zip([0.1, 0.2, 0.3, 0.25]) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
        for (actual, expected) in slice.into_iter().zip([0.1, 0.2, 0.3, 0.25]) {
            assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
        }
    }

    #[test]
    fn composite_mask_uses_raw_contents_alpha_and_strict_threshold() {
        let render_with_threshold = |threshold| {
            let mut target = render_part(2, json!(0), 0);
            target["masks"] = json!([{"source": 3, "mode": "Mask"}]);
            let mut source = composite(3, vec![render_part(4, json!(1), 0)], 0.25);
            source["enabled"] = json!(false);
            source["mask_threshold"] = json!(threshold);
            source["masks"] = json!([{"source": 6, "mode": "Mask"}]);
            let mut outer_mask = render_part(6, json!(1), 0);
            outer_mask["enabled"] = json!(false);
            outer_mask["mesh"]["verts"] = json!([0, 0, 0, 8, 4, 0, 4, 8]);
            outer_mask["mesh"]["uvs"] = json!([0, 0, 0, 1, 0.5, 0, 0.5, 1]);
            render_png(
                &scene_with(vec![target, source, outer_mask]),
                &[
                    red_texture([255, 0, 0, 255]),
                    red_texture([255, 255, 255, 255]),
                ],
                8,
                8,
                [0.0, 0.0, 8.0, 8.0],
            )
            .unwrap()
        };
        assert_eq!(pixel(&render_with_threshold(1.0), 3, 3), [0, 0, 0, 0]);
        assert_eq!(pixel(&render_with_threshold(0.999), 3, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&render_with_threshold(0.999), 6, 3), [255, 0, 0, 255]);
    }

    #[test]
    fn hidden_composite_mask_keeps_disabled_children_hidden() {
        let mut target = render_part(2, json!(0), 0);
        target["masks"] = json!([{"source": 3, "mode": "Mask"}]);
        let mut left = render_part(4, json!(1), 0);
        left["mesh"]["verts"] = json!([0, 0, 0, 8, 4, 0, 4, 8]);
        left["mesh"]["uvs"] = json!([0, 0, 0, 1, 0.5, 0, 0.5, 1]);
        let mut right = render_part(5, json!(1), 0);
        right["enabled"] = json!(false);
        right["mesh"]["verts"] = json!([4, 0, 4, 8, 8, 0, 8, 8]);
        right["mesh"]["uvs"] = json!([0.5, 0, 0.5, 1, 1, 0, 1, 1]);
        let mut source = composite(3, vec![left, right], 1.0);
        source["enabled"] = json!(false);
        let png = render_png(
            &scene_with(vec![target, source]),
            &[
                red_texture([255, 0, 0, 255]),
                red_texture([255, 255, 255, 255]),
            ],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 1, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 6, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn auxiliary_textures_and_emission_do_not_change_default_albedo_png() {
        let baseline = scene(render_part(2, json!(0), 0));
        let mut enhanced = render_part(2, json!(0), 0);
        enhanced["textures"] = json!([0, 1, 2]);
        enhanced["emissionStrength"] = json!(4.0);
        let enhanced = scene(enhanced);
        let textures = [
            red_texture([64, 0, 0, 128]),
            red_texture([0, 255, 0, 255]),
            red_texture([0, 0, 255, 255]),
        ];
        let bounds = [0.0, 0.0, 8.0, 8.0];
        let baseline = render_png(&baseline, &textures, 8, 8, bounds).unwrap();
        let enhanced = render_png(&enhanced, &textures, 8, 8, bounds).unwrap();
        assert_eq!(enhanced, baseline);

        let mut unknown_slot = render_part(2, json!(0), 0);
        unknown_slot["textures"] = json!([0, 1, 2, 3]);
        assert!(render_png(&scene(unknown_slot), &textures, 8, 8, bounds)
            .unwrap_err()
            .contains("unsupported texture slot 3"));
    }

    #[test]
    fn deforms_origin_seeded_composite_quad_after_child_transforms() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([2, 2, 2, 6, 6, 2, 6, 6]);
        let mut group = composite(10, vec![child], 1.0);
        group["transform"]["trans"] = json!([2, 0, 0]);
        let deformations =
            BTreeMap::from([(10, vec![[0.0, 0.0], [0.0, 0.0], [2.0, 0.0], [2.0, 0.0]])]);
        let png = render_png_with(
            &scene(group),
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            12,
            8,
            [0.0, 0.0, 12.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 3, 3), [0, 0, 0, 0]);
        assert_d_composite_pixel(&png, 4, 3, 88);
        assert_d_composite_pixel(&png, 9, 3, 0);
        assert_d_composite_pixel(&png, 5, 3, 199);
        assert_d_composite_pixel(&png, 8, 3, 207);
        assert_eq!(pixel(&png, 10, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn renders_offscreen_composite_content_after_quad_translation() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([-6, 2, -6, 6, -2, 2, -2, 6]);
        let model = scene(composite(10, vec![child], 1.0));
        let deformations = BTreeMap::from([(10, vec![[8.0, 0.0]; 4])]);
        let png = render_png_with(
            &model,
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 1, 3), [0, 0, 0, 0]);
        assert_eq!(pixel(&png, 3, 3), [255, 0, 0, 255]);
        assert_d_composite_pixel(&png, 5, 3, 107);
        assert_eq!(pixel(&png, 6, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn outer_composite_bounds_use_nested_source_quad_before_deformation() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([-6, 2, -6, 6, -2, 2, -2, 6]);
        let nested = composite(11, vec![child], 1.0);
        let model = scene(composite(10, vec![nested], 1.0));
        let deformations = BTreeMap::from([(10, vec![[4.0, 0.0]; 4]), (11, vec![[4.0, 0.0]; 4])]);
        let png = render_png_with(
            &model,
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            12,
            8,
            [0.0, 0.0, 12.0, 8.0],
        )
        .unwrap();
        assert_d_composite_pixel(&png, 1, 3, 37);
        assert_d_composite_pixel(&png, 3, 3, 252);
    }

    #[test]
    fn composite_source_bounds_include_child_transform_origin() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([10, 10, 10, 14, 14, 10, 14, 14]);
        child["mesh"]["origin"] = json!([10, 10]);
        let model = scene(composite(10, vec![child], 1.0));
        let deformations = BTreeMap::from([(10, vec![[1.0, 0.0]; 4])]);
        let png = render_png_with(
            &model,
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 2, 2), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 6, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn composite_source_surface_matches_projectable_texture_geometry() {
        assert_eq!(
            composite_source_surface(10, [0.0, 0.0, 6.25, 6.25]).unwrap(),
            (8, 8, [-0.875, -0.875, 7.125, 7.125])
        );
        assert_eq!(
            composite_source_surface(10, [-0.5, 0.0, 6.75, 6.25]).unwrap(),
            (9, 8, [-1.375, -0.875, 7.625, 7.125])
        );
        assert!(composite_source_surface(10, [2.0, 0.0, 1.0, 1.0]).is_err());
    }

    #[test]
    fn warped_fractional_composite_matches_d_linear_sampling_pixels() {
        // The D linear-sampling capture isolates bilinear parity from its
        // implementation-dependent default anisotropic filter.
        const D_ALPHA: [[u8; 5]; 4] = [
            [76, 117, 122, 104, 23],
            [156, 243, 253, 215, 47],
            [149, 232, 242, 206, 45],
            [59, 91, 95, 81, 18],
        ];
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([2.25, 2.25, 2.25, 6.25, 6.25, 2.25, 6.25, 6.25]);
        let deformations =
            BTreeMap::from([(10, vec![[0.0, 0.0], [0.0, 0.0], [2.0, 0.0], [2.0, 0.0]])]);
        let image = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut texture_bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(image)
            .write_to(&mut texture_bytes, ImageFormat::Png)
            .unwrap();
        let png = render_png_with(
            &scene(composite(10, vec![child], 1.0)),
            &deformations,
            &[TextureBlob {
                tag: 0,
                data: texture_bytes.into_inner(),
            }],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        for y in 0..16 {
            for x in 0..16 {
                let expected = if (3..8).contains(&x) && (2..6).contains(&y) {
                    D_ALPHA[(y - 2) as usize][(x - 3) as usize]
                } else {
                    0
                };
                let actual = pixel(&png, x, y);
                assert!(
                    actual[3].abs_diff(expected) <= 2,
                    "alpha at ({x}, {y}): {} != {expected}",
                    actual[3]
                );
                let premultiplied_red =
                    ((u16::from(actual[0]) * u16::from(actual[3]) + 127) / 255) as u8;
                assert!(
                    premultiplied_red.abs_diff(expected) <= 2,
                    "premultiplied red at ({x}, {y})"
                );
                assert_eq!(actual[1..3], [0, 0], "green/blue at ({x}, {y})");
            }
        }
    }

    #[test]
    fn origin_seeded_asymmetric_composite_matches_d_pixels() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([2, 2, 2, 6, 6, 2, 6, 6]);
        let mut group = composite(10, vec![child], 1.0);
        group["transform"]["trans"] = json!([2, 0, 0]);
        let deformations =
            BTreeMap::from([(10, vec![[0.0, 0.0], [0.0, 0.0], [2.0, 0.0], [2.0, 0.0]])]);
        let png = render_png_with(
            &scene(group),
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_d_composite_alpha_patch(
            &png,
            4,
            1,
            [
                [11, 25, 32, 32, 26],
                [62, 141, 181, 181, 147],
                [88, 199, 255, 255, 207],
                [88, 199, 255, 255, 207],
                [7, 17, 21, 21, 17],
            ],
        );
    }

    #[test]
    fn offscreen_translated_composite_matches_d_pixels() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([-6, 2, -6, 6, -2, 2, -2, 6]);
        let deformations = BTreeMap::from([(10, vec![[8.0, 0.0]; 4])]);
        let model = scene(composite(10, vec![child], 1.0));
        let png = render_png_with(
            &model,
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_d_composite_alpha_patch(
            &png,
            2,
            1,
            [
                [17, 32, 32, 13],
                [98, 181, 181, 76],
                [138, 255, 255, 107],
                [138, 255, 255, 107],
                [11, 21, 21, 9],
            ],
        );
    }

    #[test]
    fn nested_offscreen_composites_match_d_pixels() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([-6, 2, -6, 6, -2, 2, -2, 6]);
        let inner = composite(11, vec![child], 1.0);
        let deformations = BTreeMap::from([(10, vec![[4.0, 0.0]; 4]), (11, vec![[4.0, 0.0]; 4])]);
        let png = render_png_with(
            &scene(composite(10, vec![inner], 1.0)),
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_d_composite_alpha_patch(
            &png,
            1,
            1,
            [
                [4, 18, 26],
                [25, 116, 167],
                [37, 176, 252],
                [28, 132, 189],
                [2, 10, 14],
            ],
        );
    }

    #[test]
    fn nested_fractional_composites_match_d_linear_pixels() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([2.25, 2.25, 2.25, 6.25, 6.25, 2.25, 6.25, 6.25]);
        let inner = composite(11, vec![child], 1.0);
        let deformations = BTreeMap::from([(10, vec![[2.0, 0.0]; 4]), (11, vec![[1.0, 0.0]; 4])]);
        let texture = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut texture_bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(texture)
            .write_to(&mut texture_bytes, ImageFormat::Png)
            .unwrap();
        let png = render_png_with(
            &scene(composite(10, vec![inner], 1.0)),
            &deformations,
            &[TextureBlob {
                tag: 0,
                data: texture_bytes.into_inner(),
            }],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_d_composite_alpha_patch(
            &png,
            4,
            2,
            [
                [4, 50, 100, 82, 0],
                [8, 119, 241, 196, 0],
                [6, 90, 181, 147, 0],
                [1, 14, 27, 22, 0],
            ],
        );
    }

    #[test]
    fn nested_subpixel_composite_extent_matches_d_linear_pixels() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([2.25, 2.25, 2.25, 6.25, 6.25, 2.25, 6.25, 6.25]);
        let inner = composite(11, vec![child], 1.0);
        let deformations = BTreeMap::from([(10, vec![[2.0, 0.0]; 4]), (11, vec![[0.1, 0.0]; 4])]);
        let texture = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut texture_bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(texture)
            .write_to(&mut texture_bytes, ImageFormat::Png)
            .unwrap();
        let png = render_png_with(
            &scene(composite(10, vec![inner], 1.0)),
            &deformations,
            &[TextureBlob {
                tag: 0,
                data: texture_bytes.into_inner(),
            }],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_d_composite_alpha_patch(
            &png,
            4,
            2,
            [
                [37, 104, 83, 17],
                [89, 250, 200, 41],
                [67, 188, 150, 31],
                [10, 28, 23, 5],
            ],
        );
    }

    #[test]
    fn deformed_part_expands_a_warped_composite_like_d_linear_pixels() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([2.25, 2.25, 2.25, 6.25, 6.25, 2.25, 6.25, 6.25]);
        let deformations = BTreeMap::from([
            (2, vec![[0.0, 0.0], [0.0, 0.0], [2.0, 0.0], [2.0, 0.0]]),
            (10, vec![[1.0, 0.0]; 4]),
        ]);
        let texture = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut texture_bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(texture)
            .write_to(&mut texture_bytes, ImageFormat::Png)
            .unwrap();
        let png = render_png_with(
            &scene(composite(10, vec![child], 1.0)),
            &deformations,
            &[TextureBlob {
                tag: 0,
                data: texture_bytes.into_inner(),
            }],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_d_composite_alpha_patch(
            &png,
            3,
            2,
            [
                [41, 107, 122, 122, 109, 36],
                [84, 221, 253, 253, 225, 73],
                [80, 211, 242, 242, 215, 70],
                [32, 83, 95, 95, 85, 28],
            ],
        );
    }

    #[test]
    fn nonuniform_nested_composite_extents_match_d_linear_pixels() {
        let mut child = render_part(2, json!(0), 0);
        child["mesh"]["verts"] = json!([2.25, 2.25, 2.25, 6.25, 6.25, 2.25, 6.25, 6.25]);
        let inner = composite(11, vec![child], 1.0);
        let deformations = BTreeMap::from([
            (10, vec![[2.0, 0.0]; 4]),
            (11, vec![[-1.0, 0.0], [1.0, 1.0], [1.0, -1.0], [-1.0, 0.0]]),
        ]);
        let texture = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut texture_bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(texture)
            .write_to(&mut texture_bytes, ImageFormat::Png)
            .unwrap();
        let png = render_png_with(
            &scene(composite(10, vec![inner], 1.0)),
            &deformations,
            &[TextureBlob {
                tag: 0,
                data: texture_bytes.into_inner(),
            }],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_d_composite_alpha_patch(
            &png,
            4,
            1,
            [
                [0, 0, 2, 7],
                [12, 84, 127, 61],
                [1, 147, 217, 29],
                [0, 177, 162, 0],
                [0, 81, 37, 0],
            ],
        );
    }

    #[test]
    fn sibling_nested_extents_share_the_parent_integer_shrink_gate() {
        let mut left_part = render_part(2, json!(0), 0);
        left_part["mesh"]["verts"] = json!([2.25, 2.25, 2.25, 6.25, 6.25, 2.25, 6.25, 6.25]);
        let left = composite(11, vec![left_part], 1.0);

        let mut right_part = render_part(3, json!(0), 0);
        right_part["mesh"]["verts"] = json!([2.25, 2.25, 2.25, 6.25, 6.25, 2.25, 6.25, 6.25]);
        let mut right = composite(12, vec![right_part], 1.0);
        right["transform"]["trans"] = json!([7, 0, 0]);

        let deformations = BTreeMap::from([
            (10, vec![[1.0, 0.0]; 4]),
            (11, vec![[-0.6, 0.0]; 4]),
            (12, vec![[0.6, 0.0]; 4]),
        ]);
        let texture = RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut texture_bytes = Cursor::new(Vec::new());
        DynamicImage::ImageRgba8(texture)
            .write_to(&mut texture_bytes, ImageFormat::Png)
            .unwrap();
        let png = render_png_with(
            &scene(composite(10, vec![left, right], 1.0)),
            &deformations,
            &[TextureBlob {
                tag: 0,
                data: texture_bytes.into_inner(),
            }],
            16,
            16,
            [0.0, 0.0, 16.0, 16.0],
        )
        .unwrap();
        assert_d_composite_alpha_patch(
            &png,
            3,
            2,
            [
                [67, 105, 83, 11, 0, 0, 0, 36, 94, 100, 36],
                [160, 252, 199, 27, 0, 0, 0, 87, 226, 240, 88],
                [120, 189, 148, 20, 0, 0, 0, 65, 170, 180, 66],
                [18, 29, 23, 3, 0, 0, 0, 10, 26, 27, 10],
            ],
        );
    }

    #[test]
    fn composite_mask_uses_warped_raw_contents_alpha() {
        let mut target = render_part(2, json!(0), 0);
        target["masks"] = json!([{"source": 3, "mode": "Mask"}]);
        let mut child = render_part(4, json!(1), 0);
        child["mesh"]["verts"] = json!([0, 0, 0, 8, 4, 0, 4, 8]);
        let nested = composite(5, vec![child], 1.0);
        let mut source = composite(3, vec![nested], 0.25);
        source["enabled"] = json!(false);
        source["mask_threshold"] = json!(0.5);
        let deformations = BTreeMap::from([(3, vec![[2.0, 0.0]; 4])]);
        let png = render_png_with(
            &scene_with(vec![target, source]),
            &deformations,
            &[
                red_texture([255, 0, 0, 255]),
                red_texture([255, 255, 255, 255]),
            ],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 1, 3), [0, 0, 0, 0]);
        assert_eq!(pixel(&png, 3, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 6, 3), [0, 0, 0, 0]);
    }

    #[test]
    fn applies_parent_matrix_before_part_local_transform() {
        let mut child = part(json!([0, 1, 2, 2, 1, 3]));
        child["mesh"]["verts"] = json!([0, 0, 0, 1, 1, 0, 1, 1]);
        child["mesh"]["uvs"] = json!([0, 0, 0, 1, 1, 0, 1, 1]);
        child["transform"]["trans"] = json!([1, 0, 0]);
        let mut root = scene(child);
        root["nodes"]["transform"]["scale"] = json!([2, 2]);
        let png = render_png(
            &root,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 2, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 3, 1), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 1, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn rig_nodes_do_not_fail_part_raster_or_reject_their_lock_to_root() {
        let mut part = part(json!([0, 1, 2, 0, 2, 3]));
        part["uuid"] = json!(3);
        let rig = json!({
            "uuid": 4, "type": "DepthRigRoot", "enabled": true, "lockToRoot": true,
            "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
            "children": [{
                "uuid": 5, "type": "DepthBone", "enabled": true, "lockToRoot": true,
                "transform": {"trans": [10, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
                "children": []
            }]
        });
        let with_rig = json!({
            "nodes": {
                "uuid": 1, "type": "Node", "enabled": true,
                "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
                "children": [part, rig.clone()]
            }
        });
        let alone = scene(self::part(json!([0, 1, 2, 0, 2, 3])));
        let flat = render_png(
            &alone,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        let with = render_png(
            &with_rig,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(flat, with);
        assert!(scene_bounds(&with_rig).is_ok());
        let mut pitched = rig;
        pitched["transform"]["rot"] = json!([0.4, -0.2, 0.0]);
        pitched["children"][0]["transform"]["rot"] = json!([0.1, 0.2, 0.0]);
        let pitched_scene = json!({
            "nodes": {
                "uuid": 1, "type": "Node", "enabled": true,
                "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
                "children": [self::part(json!([0, 1, 2, 0, 2, 3])), pitched]
            }
        });
        let pitched_png = render_png(
            &pitched_scene,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pitched_png, flat);
        assert!(scene_bounds(&pitched_scene).is_ok());
        let mut locked_part = self::part(json!([0, 1, 2, 0, 2, 3]));
        locked_part["lockToRoot"] = json!(true);
        let locked_png = render_png(
            &scene(locked_part),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(locked_png, flat);
        let mut pinned = self::part(json!([0, 1, 2, 0, 2, 3]));
        pinned["pinToMesh"] = json!(true);
        assert!(render_png(
            &scene(pinned),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0]
        )
        .unwrap_err()
        .contains("pinToMesh parent must be a Part"));
    }

    #[test]
    fn pin_to_mesh_follows_the_deformed_triangle_and_skips_a_miss() {
        let parent = json!({
            "uuid": 4, "type": "Part", "enabled": true, "zsort": 0,
            "textures": [u32::MAX, u32::MAX, u32::MAX],
            "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
            "mesh": {
                "verts": [0.0, 0.0, 4.0, 0.0, 0.0, 4.0],
                "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0],
                "indices": [0, 1, 2],
                "origin": [0.0, 0.0]
            }
        });
        let child = |translation: [f64; 3], pin: bool| {
            json!({
                "uuid": 5, "type": "Part", "enabled": true, "zsort": 0, "pinToMesh": pin,
                "transform": {"trans": translation, "rot": [0, 0, 0], "scale": [1, 1]},
                "textures": [u32::MAX, u32::MAX, u32::MAX],
                "mesh": {
                    "verts": [0.0, 0.0, 10.0, 0.0, 10.0, 0.2, 0.0, 0.2],
                    "uvs": [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, 1.0],
                    "indices": [0, 1, 2, 0, 2, 3],
                    "origin": [0.0, 0.0]
                }
            })
        };
        let scene = |translation: [f64; 3], pin: bool| {
            let mut host = parent.clone();
            host["children"] = json!([child(translation, pin)]);
            json!({
                "nodes": {
                    "uuid": 1, "type": "Node", "enabled": true, "zsort": 0,
                    "transform": {"trans": [0, 0, 0], "rot": [0, 0, 0], "scale": [1, 1]},
                    "children": [host]
                }
            })
        };
        let mut deformations = BTreeMap::new();
        deformations.insert(4, vec![[0.0, 0.0], [8.0, 0.0], [0.0, 0.0]]);
        let unpinned = scene_bounds_with(&scene([1.0, 1.0, 0.0], false), &deformations).unwrap();
        let pinned = scene_bounds_with(&scene([1.0, 1.0, 0.0], true), &deformations).unwrap();
        assert!((unpinned[2] - 13.2).abs() < 1e-9, "{unpinned:?}");
        assert!((pinned[2] - 14.3).abs() < 1e-9, "{pinned:?}");
        // A 90° rotation of the triangle around (1, 1). The horizontal child
        // bar must swing to +Y, so max Y grows and max X collapses.
        let mut turned = BTreeMap::new();
        turned.insert(4, vec![[2.0, 0.0], [-2.0, 4.0], [-2.0, -4.0]]);
        let flat = scene_bounds_with(&scene([1.0, 1.0, 0.0], false), &turned).unwrap();
        let upright = scene_bounds_with(&scene([1.0, 1.0, 0.0], true), &turned).unwrap();
        assert!((flat[2] - 12.3).abs() < 1e-9, "{flat:?}");
        assert!((flat[3] - 4.4).abs() < 1e-9, "{flat:?}");
        assert!((upright[2] - 2.4).abs() < 1e-9, "{upright:?}");
        assert!((upright[3] - 12.1).abs() < 1e-9, "{upright:?}");
        // lockToRoot composes with the puppet root. The pin delta stays in
        // parent-local X and is added to the local translation, so a parent
        // yaw of π/2 does not turn that delta into world Y.
        let mut locked = scene([9.0, 1.0, 0.0], true);
        locked["nodes"]["children"][0]["transform"]["trans"] = json!([10.0, 0.0, 0.0]);
        locked["nodes"]["children"][0]["transform"]["rot"] =
            json!([0.0, 0.0, std::f64::consts::FRAC_PI_2]);
        locked["nodes"]["children"][0]["children"][0]["lockToRoot"] = json!(true);
        let mut locked_free = locked.clone();
        locked_free["nodes"]["children"][0]["children"][0]["pinToMesh"] = json!(false);
        let locked_pinned = scene_bounds_with(&locked, &deformations).unwrap();
        let locked_open = scene_bounds_with(&locked_free, &deformations).unwrap();
        assert!((locked_open[2] - 20.3).abs() < 1e-9, "{locked_open:?}");
        assert!((locked_pinned[2] - 22.5).abs() < 1e-9, "{locked_pinned:?}");
        assert!(
            (locked_pinned[3] - locked_open[3]).abs() < 1e-9,
            "{locked_pinned:?} {locked_open:?}"
        );
        let mut bare = scene([1.0, 1.0, 0.0], true);
        bare["nodes"]["children"][0]["mesh"]["indices"] = json!([]);
        let mut non_finite = BTreeMap::new();
        non_finite.insert(4, vec![[0.0, 0.0], [f64::NAN, 0.0], [0.0, 0.0]]);
        assert!(scene_bounds_with(&bare, &non_finite)
            .unwrap_err()
            .contains("not finite"));
        let mut overflow = BTreeMap::new();
        overflow.insert(4, vec![[1.0e308, 0.0], [0.0, 0.0], [0.0, 0.0]]);
        let mut huge = scene([-10.0, -10.0, 0.0], true);
        huge["nodes"]["children"][0]["mesh"]["verts"][0] = json!(1.0e308);
        assert!(scene_bounds_with(&huge, &overflow)
            .unwrap_err()
            .contains("not finite"));
        let mut alone = scene([0.0, 0.0, 0.0], false);
        alone["nodes"]["children"][0]["children"] = json!([]);
        alone["nodes"]["children"][0]["pinToMesh"] = json!(false);
        alone["nodes"]["children"][0]["mesh"]["verts"][0] = json!(1.0e308);
        assert!(scene_bounds_with(&alone, &overflow)
            .unwrap_err()
            .contains("non-finite"));
        assert!(
            render_png_with(&alone, &overflow, &[], 4, 4, [-2.0, -2.0, 16.0, 8.0])
                .unwrap_err()
                .contains("deformed vertex is not finite")
        );
        let outside = scene_bounds_with(&scene([10.0, 10.0, 0.0], true), &deformations).unwrap();
        let outside_free =
            scene_bounds_with(&scene([10.0, 10.0, 0.0], false), &deformations).unwrap();
        assert_eq!(outside, outside_free);
        let mut leading_degenerate = scene([1.0, 1.0, 0.0], true);
        leading_degenerate["nodes"]["children"][0]["mesh"]["indices"] = json!([0, 0, 0, 0, 1, 2]);
        let skipped = scene_bounds_with(&leading_degenerate, &deformations).unwrap();
        assert!((skipped[2] - 14.3).abs() < 1e-9, "{skipped:?}");
        let mut dangling = scene([1.0, 1.0, 0.0], true);
        dangling["nodes"]["children"][0]["mesh"]["verts"] = json!([]);
        dangling["nodes"]["children"][0]["mesh"]["uvs"] = json!([]);
        dangling["nodes"]["children"][0]["mesh"]["indices"] = json!([0, 1, 2]);
        assert!(scene_bounds_with(&dangling, &BTreeMap::new())
            .unwrap_err()
            .contains("out of range"));
        let mut tiny = scene([2.0e-8, 2.0e-8, 0.0], true);
        tiny["nodes"]["children"][0]["mesh"]["verts"] = json!([0.0, 0.0, 1.0e-7, 0.0, 0.0, 1.0e-7]);
        tiny["nodes"]["children"][0]["mesh"]["uvs"] = json!([0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        let mut tiny_deform = BTreeMap::new();
        tiny_deform.insert(4, vec![[0.0, 0.0], [10.0, 0.0], [0.0, 0.0]]);
        let tiny_pinned = scene_bounds_with(&tiny, &tiny_deform).unwrap();
        let mut tiny_free = tiny.clone();
        tiny_free["nodes"]["children"][0]["children"][0]["pinToMesh"] = json!(false);
        let tiny_open = scene_bounds_with(&tiny_free, &tiny_deform).unwrap();
        assert!(
            tiny_pinned[2] > tiny_open[2] + 1.0,
            "{tiny_pinned:?} {tiny_open:?}"
        );
        let mut only_degenerate = scene([1.0, 1.0, 0.0], true);
        only_degenerate["nodes"]["children"][0]["mesh"]["indices"] = json!([0, 0, 0]);
        let mut only_free = only_degenerate.clone();
        only_free["nodes"]["children"][0]["children"][0]["pinToMesh"] = json!(false);
        assert_eq!(
            scene_bounds_with(&only_degenerate, &deformations).unwrap(),
            scene_bounds_with(&only_free, &deformations).unwrap()
        );
        let mut no_uv = scene([1.0, 1.0, 0.0], true);
        no_uv["nodes"]["children"][0]["mesh"]
            .as_object_mut()
            .unwrap()
            .remove("uvs");
        let without_uv = scene_bounds_with(&no_uv, &deformations).unwrap();
        assert!((without_uv[2] - 14.3).abs() < 1e-9, "{without_uv:?}");
        assert!(render_png_with(&no_uv, &deformations, &[], 4, 4, [-2.0, -2.0, 16.0, 8.0]).is_ok());
        let mut textured = no_uv.clone();
        textured["nodes"]["children"][0]["children"][0]["textures"] =
            json!([0u64, u32::MAX, u32::MAX]);
        textured["nodes"]["children"][0]["children"][0]["mesh"]
            .as_object_mut()
            .unwrap()
            .remove("uvs");
        assert!(render_png_with(
            &textured,
            &deformations,
            &[red_texture([255, 0, 0, 255])],
            4,
            4,
            [-2.0, -2.0, 16.0, 8.0],
        )
        .unwrap_err()
        .contains("uvs"));
        let mut flat_parent = scene([1.0, 1.0, 0.0], true);
        flat_parent["nodes"]["children"][0]["transform"]["scale"] = json!([0.0, 1.0]);
        assert!(scene_bounds_with(&flat_parent, &deformations).is_ok());
        let vertices_only = json!({
            "nodes": {
                "uuid": 1, "type": "Node", "enabled": true, "zsort": 0,
                "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                "children": [{
                    "uuid": 4, "type": "Part", "enabled": true, "zsort": 0, "pinToMesh": false,
                    "textures": [u32::MAX, u32::MAX, u32::MAX],
                    "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                    "vertices": [[0.0, 0.0], [4.0, 0.0], [0.0, 4.0]],
                    "uvs": [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]],
                    "indices": [0, 1, 2],
                    "children": [{
                        "uuid": 5, "type": "Part", "enabled": true, "zsort": 0, "pinToMesh": true,
                        "textures": [u32::MAX, u32::MAX, u32::MAX],
                        "transform": {"trans": [1.0, 1.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
                        "vertices": [[0.0, 0.0], [10.0, 0.0], [10.0, 0.2], [0.0, 0.2]],
                        "uvs": [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
                        "indices": [0, 1, 2, 0, 2, 3]
                    }]
                }]
            }
        });
        let from_vertices = scene_bounds_with(&vertices_only, &deformations).unwrap();
        assert!((from_vertices[2] - 14.3).abs() < 1e-9, "{from_vertices:?}");
        assert!(render_png_with(
            &vertices_only,
            &deformations,
            &[],
            4,
            4,
            [-2.0, -2.0, 16.0, 8.0],
        )
        .is_ok());
        let held = scene([1.0, 1.0, 0.0], true);
        let mut broken_deform = deformations.clone();
        broken_deform.insert(4, vec![[f64::NAN, 0.0], [8.0, 0.0], [0.0, 0.0]]);
        assert!(scene_bounds_with(&held, &broken_deform)
            .unwrap_err()
            .contains("is not finite"));
        let mut bad_index = held.clone();
        bad_index["nodes"]["children"][0]["mesh"]["indices"] = json!([0, 1, 9]);
        assert!(scene_bounds_with(&bad_index, &deformations)
            .unwrap_err()
            .contains("out of range"));
        assert_eq!(
            held["nodes"]["children"][0]["children"][0]["transform"]["trans"],
            json!([1.0, 1.0, 0.0])
        );

        let mut session = crate::Session::new();
        let opened = session.handle_request(&json!({
            "op": "new",
            "model": {
                "nodes": held["nodes"],
                "param": [{
                    "uuid": 9, "is_vec2": false,
                    "min": [0.0, 0.0], "max": [1.0, 0.0], "defaults": [1.0, 0.0],
                    "axis_points": [[0.0, 1.0], [0.0]],
                    "bindings": [{
                        "node": 4, "param_name": "deform",
                        "values": [[
                            [[0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]
                        ], [
                            [[0.0, 0.0], [8.0, 0.0], [0.0, 0.0]]
                        ]],
                        "isSet": [[true], [true]]
                    }]
                }],
                "automation": []
            }
        }));
        assert_eq!(opened["succeeded"], true, "{opened}");
        let before = session.handle_request(&json!({"op": "snapshot"}));
        let bounds = session.handle_request(&json!({"op": "bounds"}));
        assert_eq!(bounds["succeeded"], true, "{bounds}");
        assert!(
            (bounds["result"]["bounds"][2].as_f64().unwrap() - 14.3).abs() < 1e-9,
            "{bounds}"
        );
        let drawn = session.handle_request(&json!({
            "op": "render", "width": 4, "height": 4, "bounds": [-2.0, -2.0, 16.0, 8.0]
        }));
        assert_eq!(drawn["succeeded"], true, "{drawn}");
        assert_eq!(session.handle_request(&json!({"op": "snapshot"})), before);
        let non_part = {
            let mut model = held.clone();
            model["nodes"]["children"][0]["type"] = json!("Node");
            model
        };
        let replaced = session.handle_request(&json!({"op": "new", "model": {
            "nodes": non_part["nodes"], "param": [], "automation": []
        }}));
        assert_eq!(replaced["succeeded"], true, "{replaced}");
        let before_fail = session.handle_request(&json!({"op": "snapshot"}));
        let failed = session.handle_request(&json!({
            "op": "bounds"
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"]
            .as_str()
            .unwrap_or("")
            .contains("pinToMesh parent must be a Part"));
        assert_eq!(
            session.handle_request(&json!({"op": "snapshot"})),
            before_fail
        );
    }

    fn shifted_part(id: u64, translation: [f64; 3], lock_to_root: bool) -> Value {
        let mut node = part(json!([0, 1, 2, 0, 2, 3]));
        node["uuid"] = json!(id);
        node["transform"]["trans"] = json!(translation);
        node["lockToRoot"] = json!(lock_to_root);
        node
    }

    fn group(id: u64, translation: [f64; 3], lock_to_root: bool, children: Vec<Value>) -> Value {
        json!({
            "uuid": id, "type": "Node", "enabled": true, "zsort": 0,
            "lockToRoot": lock_to_root,
            "transform": {"trans": translation, "rot": [0, 0, 0], "scale": [1, 1]},
            "children": children
        })
    }

    #[test]
    fn part_lock_to_root_uses_the_puppet_root_not_the_parent() {
        let root_shift = [100.0, 0.0, 0.0];
        let parent_shift = [50.0, 0.0, 0.0];
        let locked_under_parent = json!({
            "nodes": group(1, root_shift, false, vec![
                group(4, parent_shift, false, vec![shifted_part(2, [0.0, 0.0, 0.0], true)])
            ])
        });
        let locked_on_root = json!({
            "nodes": group(1, root_shift, false, vec![shifted_part(2, [0.0, 0.0, 0.0], true)])
        });
        let unlocked = json!({
            "nodes": group(1, root_shift, false, vec![
                group(4, parent_shift, false, vec![shifted_part(2, [0.0, 0.0, 0.0], false)])
            ])
        });
        let local_only = json!({
            "nodes": group(1, [0.0, 0.0, 0.0], false, vec![shifted_part(2, [0.0, 0.0, 0.0], true)])
        });
        let locked_bounds = scene_bounds(&locked_under_parent).unwrap();
        assert_eq!(locked_bounds, scene_bounds(&locked_on_root).unwrap());
        let unlocked_bounds = scene_bounds(&unlocked).unwrap();
        assert!((locked_bounds[0] - unlocked_bounds[0]).abs() > 20.0);
        assert!((locked_bounds[0] - scene_bounds(&local_only).unwrap()[0]).abs() > 20.0);

        let inherited = json!({
            "nodes": group(1, root_shift, false, vec![
                group(4, parent_shift, false, vec![
                    group(5, [20.0, 0.0, 0.0], true, vec![
                        shifted_part(2, [0.0, 0.0, 0.0], false)
                    ])
                ])
            ])
        });
        let locked_child = json!({
            "nodes": group(1, root_shift, false, vec![
                group(4, parent_shift, false, vec![
                    group(5, [20.0, 0.0, 0.0], true, vec![
                        shifted_part(3, [0.0, 0.0, 0.0], true)
                    ])
                ])
            ])
        });
        let inherited_bounds = scene_bounds(&inherited).unwrap();
        let locked_child_bounds = scene_bounds(&locked_child).unwrap();
        assert_eq!(locked_child_bounds, locked_bounds);
        assert!((inherited_bounds[0] - locked_child_bounds[0]).abs() > 10.0);
        assert!((inherited_bounds[0] - unlocked_bounds[0]).abs() > 10.0);
    }

    #[test]
    fn part_raster_projects_xy_rotation_and_ignores_translation_z() {
        let flat = render_png(
            &scene(part(json!([0, 1, 2, 0, 2, 3]))),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        let mut pitched = part(json!([0, 1, 2, 0, 2, 3]));
        pitched["transform"]["rot"] = json!([std::f64::consts::FRAC_PI_2, 0.0, 0.0]);
        let collapsed = render_png(
            &scene(pitched),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_ne!(flat, collapsed);
        assert_eq!(pixel(&collapsed, 3, 3), [0, 0, 0, 0]);
        let mut yawed = part(json!([0, 1, 2, 0, 2, 3]));
        yawed["transform"]["rot"] = json!([0.0, 0.0, 0.3]);
        let spun = render_png(
            &scene(yawed.clone()),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        let (sin, cos) = 0.3_f64.sin_cos();
        let legacy = [cos * 4.0 - sin * 4.0, sin * 4.0 + cos * 4.0];
        let projected = project(
            bone_deform::projection_matrix([0.0, 0.0, 9.0], [0.0, 0.0, 0.3], [1.0, 1.0]).unwrap(),
            [4.0, 4.0],
        )
        .unwrap();
        assert!(
            (projected[0] - legacy[0]).abs() < 1e-9,
            "{projected:?} {legacy:?}"
        );
        assert!(
            (projected[1] - legacy[1]).abs() < 1e-9,
            "{projected:?} {legacy:?}"
        );
        assert_ne!(spun, flat);
        let mut raised = part(json!([0, 1, 2, 0, 2, 3]));
        raised["transform"]["trans"] = json!([0, 0, 9]);
        let raised = render_png(
            &scene(raised),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(flat, raised);
        assert!(
            bone_deform::projection_matrix([0.0, 0.0, 0.0], [f64::NAN, 0.0, 0.0], [1.0, 1.0])
                .is_err()
        );
        let mut pitched_parent = json!({
            "uuid": 1, "type": "Node", "enabled": true,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [std::f64::consts::FRAC_PI_2, 0.0, 0.0], "scale": [1.0, 1.0]},
            "children": []
        });
        let mut deep = part(json!([0, 1, 2, 0, 2, 3]));
        deep["transform"]["trans"] = json!([0.0, 0.0, 4.0]);
        deep["mesh"]["verts"] = json!([0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
        pitched_parent["children"] = json!([deep]);
        let swung = scene_bounds(&json!({"nodes": pitched_parent})).unwrap();
        let mut flat_parent = json!({
            "uuid": 1, "type": "Node", "enabled": true,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [std::f64::consts::FRAC_PI_2, 0.0, 0.0], "scale": [1.0, 1.0]},
            "children": []
        });
        let mut shallow = part(json!([0, 1, 2, 0, 2, 3]));
        shallow["mesh"]["verts"] = json!([0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0]);
        flat_parent["children"] = json!([shallow]);
        let unswung = scene_bounds(&json!({"nodes": flat_parent})).unwrap();
        assert!(
            (swung[1] - unswung[1]).abs() > 1.0,
            "ancestor pitch should swing child Z into Y: {swung:?} {unswung:?}"
        );
        let flat_bounds = scene_bounds(&scene(part(json!([0, 1, 2, 0, 2, 3])))).unwrap();
        let mut edge = part(json!([0, 1, 2, 0, 2, 3]));
        edge["transform"]["rot"] = json!([std::f64::consts::FRAC_PI_2, 0.0, 0.0]);
        let collapsed_bounds = scene_bounds(&scene(edge)).unwrap();
        assert!(
            collapsed_bounds[3] < flat_bounds[3],
            "{collapsed_bounds:?} {flat_bounds:?}"
        );
    }

    #[test]
    fn renders_geometric_masks_without_uvs_and_ignores_mask_bounds() {
        let mut target = part(json!([0, 1, 2, 2, 1, 3]));
        target["masks"] = json!([{"source": 3, "mode": "Mask"}]);
        let source = mask(3, json!([0, 0, 0, 8, 8, 0]), json!([0, 1, 2]));
        let mut remote = mask(4, json!([0, 0, 0, 8, 8, 0]), json!([0, 1, 2]));
        remote["transform"]["trans"] = json!([100, 100, 0]);
        let masked_scene = scene_with(vec![target, source, remote]);
        let png = render_png(
            &masked_scene,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 1, 1), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 6, 6), [0, 0, 0, 0]);
        assert_eq!(
            scene_bounds(&masked_scene).unwrap(),
            scene_bounds(&scene(part(json!([0, 1, 2, 2, 1, 3])))).unwrap()
        );
    }

    #[test]
    fn part_mask_threshold_uses_albedo_alpha_without_opacity_or_tint() {
        for (threshold, visible) in [(0.49, true), (128.0 / 255.0, false), (0.52, false)] {
            let mut target = part(json!([0, 1, 2, 2, 1, 3]));
            target["masks"] = json!([{"source": 3, "mode": "Mask"}]);
            let mut source = part(json!([0, 1, 2, 2, 1, 3]));
            source["uuid"] = json!(3);
            source["enabled"] = json!(false);
            source["textures"] = json!([1u64, u32::MAX, u32::MAX]);
            source["mask_threshold"] = json!(threshold);
            source["opacity"] = json!(0.0);
            source["tint"] = json!([0.0, 0.0, 0.0]);
            let png = render_png(
                &scene_with(vec![target, source]),
                &[red_texture([255, 0, 0, 255]), red_texture([0, 255, 0, 128])],
                8,
                8,
                [0.0, 0.0, 8.0, 8.0],
            )
            .unwrap();
            assert_eq!(
                pixel(&png, 1, 1),
                if visible {
                    [255, 0, 0, 255]
                } else {
                    [0, 0, 0, 0]
                }
            );
        }
    }

    #[test]
    fn applies_ordered_dodge_masks_deduplicates_bindings_and_prefers_modern_masks() {
        let full = mask(
            3,
            json!([0, 0, 0, 8, 8, 0, 8, 8]),
            json!([0, 1, 2, 2, 1, 3]),
        );
        let half = mask(4, json!([0, 0, 0, 8, 8, 0]), json!([0, 1, 2]));
        let textures = [red_texture([255, 0, 0, 255])];
        let render = |bindings: Option<Value>, legacy: Option<Value>| {
            let mut target = part(json!([0, 1, 2, 2, 1, 3]));
            if let Some(bindings) = bindings {
                target["masks"] = bindings;
            }
            if let Some(legacy) = legacy {
                target["masked_by"] = legacy;
                target["mask_mode"] = json!("DodgeMask");
            }
            render_png(
                &scene_with(vec![target, full.clone(), half.clone()]),
                &textures,
                8,
                8,
                [0.0, 0.0, 8.0, 8.0],
            )
            .unwrap()
        };

        let normal_then_dodge = render(
            Some(json!([
                {"source": 3, "mode": "Mask"},
                {"source": 3, "mode": "Mask"},
                {"source": 4, "mode": "DodgeMask"}
            ])),
            None,
        );
        assert_eq!(pixel(&normal_then_dodge, 1, 1), [0, 0, 0, 0]);
        assert_eq!(pixel(&normal_then_dodge, 6, 6), [255, 0, 0, 255]);

        let dodge_then_normal = render(
            Some(json!([
                {"source": 4, "mode": "DodgeMask"},
                {"source": 3, "mode": "Mask"}
            ])),
            None,
        );
        assert_eq!(pixel(&dodge_then_normal, 1, 1), [255, 0, 0, 255]);

        let same_source_both_modes = render(
            Some(json!([
                {"source": 3, "mode": "Mask"},
                {"source": 3, "mode": "DodgeMask"}
            ])),
            None,
        );
        assert_eq!(pixel(&same_source_both_modes, 1, 1), [0, 0, 0, 0]);
        let legacy_mask = render(None, Some(json!([3])));
        assert_eq!(pixel(&legacy_mask, 1, 1), [0, 0, 0, 0]);
        let modern_wins = render(
            Some(json!([{"source": 3, "mode": "Mask"}])),
            Some(json!([4])),
        );
        assert_eq!(pixel(&modern_wins, 1, 1), [255, 0, 0, 255]);

        let mut unresolved = part(json!([0, 1, 2, 2, 1, 3]));
        unresolved["masks"] = json!([
            {"source": 999, "mode": "Mask"},
            {"source": 9, "mode": "Mask"}
        ]);
        let png = render_png(
            &scene_with(vec![
                unresolved,
                full,
                half,
                json!({
                    "uuid": 9, "type": "Node", "enabled": true,
                    "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]}
                }),
            ]),
            &textures,
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 1, 1), [255, 0, 0, 255]);
    }

    #[test]
    fn disabled_ancestor_transform_and_source_deformation_affect_mask_geometry() {
        let mut target = part(json!([0, 1, 2, 2, 1, 3]));
        target["masks"] = json!([{"source": 3, "mode": "Mask"}]);
        let mut parent = json!({
            "uuid": 5, "type": "Node", "enabled": false,
            "transform": {"trans": [2,0,0], "rot": [0,0,0], "scale": [1,1]},
            "children": [mask(3, json!([0, 0, 0, 8, 4, 0]), json!([0, 1, 2]))]
        });
        parent["children"][0]["enabled"] = json!(true);
        let model = scene_with(vec![target, parent]);
        let png = render_png_with(
            &model,
            &BTreeMap::from([(3, vec![[2.0, 0.0]; 3])]),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 5, 1), [255, 0, 0, 255]);
        assert_eq!(pixel(&png, 1, 1), [0, 0, 0, 0]);
        assert_eq!(
            scene_bounds(&model).unwrap(),
            scene_bounds(&scene(part(json!([0, 1, 2, 2, 1, 3])))).unwrap()
        );
    }

    #[test]
    fn disabled_weld_target_moves_visible_part_and_bounds_together() {
        let mut visible = part(json!([0, 1, 2, 2, 1, 3]));
        visible["weldedLinks"] = json!([{
            "targetUUID": 3,
            "indices": [0, 1, 2, 3],
            "weight": 0.0
        }]);
        let mut target = part(json!([0, 1, 2, 2, 1, 3]));
        target["uuid"] = json!(3);
        target["enabled"] = json!(false);
        target["transform"]["trans"] = json!([16, 0, 0]);
        target["weldedLinks"] = json!([{
            "targetUUID": 2,
            "indices": [0, 1, 2, 3],
            "weight": 0.0
        }]);
        let model = scene_with(vec![visible, target]);

        assert_eq!(scene_bounds(&model).unwrap(), [15.2, -0.8, 24.8, 8.8]);
        let png = render_png(
            &model,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [16.0, 0.0, 24.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 3, 3), [255, 0, 0, 255]);
    }

    #[test]
    fn disabled_mask_weld_target_moves_visible_part_and_bounds_together() {
        let mut visible = part(json!([0, 1, 2, 2, 1, 3]));
        visible["weldedLinks"] = json!([{
            "targetUUID": 3,
            "indices": [0, 1, 2, 3],
            "weight": 0.0
        }]);
        let mut target = mask(
            3,
            json!([0, 0, 0, 8, 8, 0, 8, 8]),
            json!([0, 1, 2, 2, 1, 3]),
        );
        target["enabled"] = json!(false);
        target["transform"]["trans"] = json!([16, 0, 0]);
        target["weldedLinks"] = json!([{
            "targetUUID": 2,
            "indices": [0, 1, 2, 3],
            "weight": 0.0
        }]);
        let model = scene_with(vec![visible, target]);

        assert_eq!(scene_bounds(&model).unwrap(), [15.2, -0.8, 24.8, 8.8]);
        let png = render_png(
            &model,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [16.0, 0.0, 24.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&png, 3, 3), [255, 0, 0, 255]);
    }

    #[test]
    fn skips_unused_disabled_invalid_pin_branch() {
        let visible = part(json!([0, 1, 2, 2, 1, 3]));
        let baseline = scene(visible.clone());
        let invalid_part = json!({
            "uuid": 3, "type": "Part", "enabled": true, "pinToMesh": true,
            "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]},
            "textures": [0u64, u32::MAX, u32::MAX],
            "mesh": {
                "verts": [0,0, 0,8, 8,0, 8,8],
                "uvs": [0,0, 0,1, 1,0, 1,1],
                "indices": [0,1,2, 2,1,3],
                "origin": [0,0]
            }
        });
        let unused = scene_with(vec![
            visible,
            json!({
                "uuid": 4, "type": "Node", "enabled": false,
                "transform": {"trans": [0,0,0], "rot": [0,0,0], "scale": [1,1]},
                "children": [invalid_part]
            }),
        ]);
        let textures = [red_texture([255, 0, 0, 255])];

        assert_eq!(
            scene_bounds(&unused).unwrap(),
            scene_bounds(&baseline).unwrap()
        );
        let baseline_png = render_png(&baseline, &textures, 8, 8, [0.0, 0.0, 8.0, 8.0]).unwrap();
        let unused_png = render_png(&unused, &textures, 8, 8, [0.0, 0.0, 8.0, 8.0]).unwrap();
        assert_eq!(unused_png, baseline_png);
    }

    #[test]
    fn rejects_malformed_meshes() {
        let malformed = part(json!([0, 1, 9]));
        assert!(render_png(
            &scene(malformed),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0]
        )
        .unwrap_err()
        .contains("index"));
    }

    #[test]
    fn deform_offsets_move_part_pixels_without_baking_vertices() {
        let base = scene(part(json!([0, 1, 2, 2, 1, 3])));
        let plain = render_png(
            &base,
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_eq!(pixel(&plain, 3, 3), [255, 0, 0, 255]);
        let moved = render_png_with(
            &base,
            &BTreeMap::from([(2, vec![[4.0, 0.0]; 4])]),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        )
        .unwrap();
        assert_ne!(plain, moved);
        assert_eq!(pixel(&moved, 3, 3), [0, 0, 0, 0]);
        assert_eq!(pixel(&moved, 5, 3), [255, 0, 0, 255]);
        let framed = scene_bounds_with(&base, &BTreeMap::from([(2, vec![[4.0, 0.0]; 4])])).unwrap();
        let plain_bounds = scene_bounds(&base).unwrap();
        assert!(framed[2] > plain_bounds[2], "{framed:?} {plain_bounds:?}");
        assert_eq!(
            base["nodes"]["children"][0]["mesh"]["verts"],
            json!([0, 0, 0, 8, 8, 0, 8, 8])
        );
        let mismatch = render_png_with(
            &base,
            &BTreeMap::from([(2, vec![[1.0, 0.0]])]),
            &[red_texture([255, 0, 0, 255])],
            8,
            8,
            [0.0, 0.0, 8.0, 8.0],
        );
        assert!(mismatch.unwrap_err().contains("length mismatch"));
        assert_eq!(
            base["nodes"]["children"][0]["mesh"]["verts"],
            json!([0, 0, 0, 8, 8, 0, 8, 8])
        );

        let mut session = crate::Session::new();
        let opened = session.handle_request(&json!({
            "op": "new",
            "model": {
                "nodes": base["nodes"],
                "automation": [],
                "param": [{
                    "uuid": 9,
                    "is_vec2": false,
                    "min": [0.0, 0.0],
                    "max": [1.0, 0.0],
                    "defaults": [0.0, 0.0],
                    "axis_points": [[0.0, 1.0], [0.0]],
                    "bindings": [{
                        "node": 2,
                        "param_name": "deform",
                        "interpolate_mode": "Stepped",
                        "values": [[[[0.0, 0.0], [0.0, 0.0], [0.0, 0.0], [0.0, 0.0]]], [[[4.0, 0.0], [4.0, 0.0], [4.0, 0.0], [4.0, 0.0]]]],
                        "isSet": [[true], [true]]
                    }]
                }]
            }
        }));
        assert_eq!(opened["succeeded"], true, "{opened}");
        let before = session.handle_request(&json!({"op":"snapshot"}));
        let failed = session.handle_request(&json!({
            "op": "render",
            "width": 8,
            "height": 8,
            "bounds": [0.0, 0.0, 8.0, 8.0],
            "values": {"9": [0.5, 0.0]}
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("unsupported"));
        assert_eq!(session.handle_request(&json!({"op":"snapshot"})), before);
        let mut linear =
            session.handle_request(&json!({"op":"snapshot"}))["result"]["model"].clone();
        linear["param"][0]["bindings"][0]["interpolate_mode"] = json!("Linear");
        linear["nodes"]["children"][0]["mesh"]["indices"] = json!([]);
        linear["nodes"]["children"][0]["indices"] = json!([]);
        linear["nodes"]["children"][0]
            .as_object_mut()
            .unwrap()
            .remove("textures");
        let reloaded = session.handle_request(&json!({"op":"new","model": linear}));
        assert_eq!(reloaded["succeeded"], true, "{reloaded}");
        let held = session.handle_request(&json!({"op":"snapshot"}));
        let drawn = session.handle_request(&json!({
            "op": "render",
            "width": 8,
            "height": 8,
            "bounds": [0.0, 0.0, 8.0, 8.0],
            "values": {"9": [0.5, 0.0]}
        }));
        assert_eq!(drawn["succeeded"], true, "{drawn}");
        let after = session.handle_request(&json!({"op":"snapshot"}));
        assert_eq!(after["result"]["model"], held["result"]["model"]);
        assert_eq!(after["result"]["revision"], held["result"]["revision"]);
        assert_eq!(after["result"]["canUndo"], held["result"]["canUndo"]);
        assert_eq!(
            after["result"]["model"]["nodes"]["children"][0]["mesh"]["verts"],
            json!([0, 0, 0, 8, 8, 0, 8, 8])
        );
    }

    #[test]
    fn renders_an_empty_scene_as_transparent_png() {
        let model = json!({
            "nodes": {"uuid":1,"type":"Node","transform":{"trans":[0,0,0],"rot":[0,0,0],"scale":[1,1]},"children":[]}
        });
        let png = render_png(&model, &[], 2, 2, [0.0, 0.0, 2.0, 2.0]).unwrap();
        assert_eq!(pixel(&png, 1, 1), [0, 0, 0, 0]);
    }
}
