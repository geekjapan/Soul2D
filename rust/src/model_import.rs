//! Layered PSD to puppet model: new-model import (`io/inimport.d`) and merge into an existing model
//! (`windows/psdmerge.d`, `ngMergePSDDefault` in `commands/puppet/file.d`).
//!
//! Everything here is pure: bytes and JSON in, JSON and PNG textures out. The PSD is untrusted.

use crate::kra;
use crate::psd::{self, BlendMode, LayerType, Limits};
use crate::render::{MAX_COMPOSITE_DEPTH, MAX_PACKET_MASK_REFERENCES, MAX_TEXTURE_PIXELS};
use serde_json::{json, Map, Value};
use std::collections::{HashMap, HashSet};
use std::io::Cursor;

const MAX_GROUP_DEPTH: usize = 48;
const MAX_TEXTURE_SIDE: u32 = 8192;
const NO_TEXTURE: u64 = u32::MAX as u64;
pub(crate) const TEXTURE_PNG: u8 = 0;
const GROUP_TYPES: [&str; 3] = ["Node", "Composite", "DynamicComposite"];

type Matrix = [[f64; 3]; 3];

/// One texture slot of an INX file. `tag` 0 is PNG, 1 is TGA; imports always produce PNG.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Texture {
    pub tag: u8,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOptions {
    /// D `IncImportSettings.keepStructure`: layer groups become nodes instead of being flattened.
    pub keep_structure: bool,
    /// Node type of a kept group: `Node`, `Composite` or `DynamicComposite`.
    pub group_node_type: String,
    /// UUID of the root; every created node takes the next number.
    pub first_id: u32,
    /// Part quads cover every texel of their layer. D halves the size with integer division, so a layer of odd
    /// width or height is drawn one pixel short; the part split needs the flattened picture exact.
    pub exact_quads: bool,
}

impl Default for ImportOptions {
    fn default() -> Self {
        Self {
            keep_structure: true,
            group_node_type: "DynamicComposite".into(),
            first_id: 1,
            exact_quads: false,
        }
    }
}

impl ImportOptions {
    /// Reads `keepStructure` (default true) and `layerGroupNodeType` (default and empty: DynamicComposite).
    pub fn from_request(request: &Value, first_id: u32) -> Result<Self, String> {
        let mut options = Self { first_id, ..Self::default() };
        match request.get("keepStructure") {
            None | Some(Value::Null) => {}
            Some(value) => options.keep_structure = value.as_bool().ok_or("keepStructure must be a boolean")?,
        }
        match request.get("layerGroupNodeType") {
            None | Some(Value::Null) => {}
            Some(value) => {
                let name = value.as_str().ok_or("layerGroupNodeType must be a string")?;
                if !name.is_empty() {
                    options.group_node_type = name.to_owned();
                }
            }
        }
        Ok(options)
    }
}

#[derive(Clone, Debug)]
pub struct ImportedModel {
    /// A complete model (`meta`, `nodes`, `param`, ...). Part `textures` hold indices into `textures`.
    pub model: Value,
    pub textures: Vec<Texture>,
    /// What the import left out or could not carry over, for the reply (not document state). Empty for a PSD
    /// and for a Krita file that lost nothing.
    pub report: Map<String, Value>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeOptions {
    /// D `renameMapped`: a replaced Part takes the layer's name.
    pub rename_mapped: bool,
    /// D `retranslateMapped`: a replaced Part moves to the layer's position on the canvas.
    pub retranslate_mapped: bool,
    /// New Parts take the first unused UUID at or above this one.
    pub first_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Match {
    /// The layer path equals the Part's `psdLayerPath` (or `/name` when it has none).
    Path,
    /// Only the last path component matches.
    Name,
}

#[derive(Clone, Debug)]
pub struct MergeLayer {
    pub name: String,
    /// `/Group/Sub/Layer`, stored as `psdLayerPath` on the Part that receives the layer.
    pub path: String,
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
    /// D's default binding: the first Part that matches, else `None` (the layer is added).
    pub target: Option<u32>,
    pub matched_by: Option<Match>,
    /// Premultiplied RGBA, PNG-encoded.
    pub texture: Texture,
    world: [f32; 2],
}

#[derive(Clone, Debug)]
pub struct MergePlan {
    pub document_size: [u32; 2],
    /// Layers with pixels, top to bottom (the order D lists them in).
    pub layers: Vec<MergeLayer>,
    /// Parts that no layer targets by default.
    pub unmapped_parts: Vec<u32>,
    pub options: MergeOptions,
    /// `skippedLayers` of a Krita file (see [`skipped_json`]); `None` for a PSD or when nothing was skipped.
    pub skipped: Option<Value>,
    /// `blendModesNotApplied` (see [`unmapped_blend_json`]); `None` when every blend mode is one the engine has.
    pub blend_modes_not_applied: Option<Value>,
}

/// The per-layer choice of the merge window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Choice {
    /// Put the layer's texture into this Part ("Use"/drag onto a Part).
    Replace(u32),
    /// Create a new Part under the root ("Unmap").
    Add,
    /// Skip the layer.
    Ignore,
}

#[derive(Clone, Debug)]
pub struct Merged {
    pub model: Value,
    pub textures: Vec<Texture>,
    pub created: Vec<u32>,
    pub replaced: Vec<u32>,
}

impl MergePlan {
    /// D's default bindings: replace the matched Part, else add.
    pub fn default_choices(&self) -> Vec<Choice> {
        self.layers
            .iter()
            .map(|layer| layer.target.map_or(Choice::Add, Choice::Replace))
            .collect()
    }

    pub fn to_json(&self) -> Value {
        let layers: Vec<Value> = self
            .layers
            .iter()
            .enumerate()
            .map(|(index, layer)| {
                json!({
                    "index": index,
                    "name": layer.name,
                    "path": layer.path,
                    "rect": [layer.left, layer.top, layer.width, layer.height],
                    "target": layer.target,
                    "matchedBy": layer.matched_by.map(|kind| match kind {
                        Match::Path => "path",
                        Match::Name => "name",
                    }),
                })
            })
            .collect();
        let mut plan = json!({
            "document": {"width": self.document_size[0], "height": self.document_size[1]},
            "layers": layers,
            "unmappedParts": self.unmapped_parts,
        });
        if let Some(skipped) = &self.skipped {
            plan["skippedLayers"] = skipped.clone();
        }
        if let Some(unmapped) = &self.blend_modes_not_applied {
            plan["blendModesNotApplied"] = unmapped.clone();
        }
        plan
    }

    /// Defaults overridden by `request.choices`: `[{layer, action: "replace"|"add"|"ignore", part?}]`.
    pub fn choices_from_request(&self, request: &Value) -> Result<Vec<Choice>, String> {
        let mut choices = self.default_choices();
        let list = match request.get("choices") {
            None | Some(Value::Null) => return Ok(choices),
            Some(value) => value.as_array().ok_or("choices must be an array")?,
        };
        let mut seen = HashSet::new();
        for entry in list {
            let layer = entry
                .get("layer")
                .and_then(Value::as_u64)
                .and_then(|layer| usize::try_from(layer).ok())
                .filter(|layer| *layer < choices.len())
                .ok_or("choice.layer must be the index of a plan layer")?;
            if !seen.insert(layer) {
                return Err(format!("layer {layer} has more than one choice"));
            }
            choices[layer] = match entry.get("action").and_then(Value::as_str) {
                Some("add") => Choice::Add,
                Some("ignore") => Choice::Ignore,
                Some("replace") => Choice::Replace(
                    entry
                        .get("part")
                        .and_then(Value::as_u64)
                        .and_then(crate::valid_id)
                        .ok_or("a replace choice needs a part UUID")?,
                ),
                _ => return Err("choice.action must be replace, add or ignore".into()),
            };
        }
        Ok(choices)
    }
}

/// The INX bytes of a model and its textures (`Document::parse` reads them back).
pub fn inx_bytes(model: &Value, textures: &[Texture]) -> Result<Vec<u8>, String> {
    let json = serde_json::to_vec(model).map_err(|error| error.to_string())?;
    let mut out = b"TRNSRTS\0".to_vec();
    out.extend_from_slice(&u32::try_from(json.len()).map_err(|_| "puppet JSON is too large")?.to_be_bytes());
    out.extend_from_slice(&json);
    out.extend_from_slice(b"TEX_SECT");
    out.extend_from_slice(&u32::try_from(textures.len()).map_err(|_| "too many textures")?.to_be_bytes());
    for texture in textures {
        out.extend_from_slice(&u32::try_from(texture.data.len()).map_err(|_| "texture is too large")?.to_be_bytes());
        out.push(texture.tag);
        out.extend_from_slice(&texture.data);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Shared PSD reading
// ---------------------------------------------------------------------------------------------

struct Budget {
    pixels: u64,
    bytes: u64,
}

impl Budget {
    fn new(limits: &Limits, used_bytes: u64) -> Result<Self, String> {
        Ok(Self {
            pixels: limits.max_pixels.saturating_mul(4),
            bytes: limits
                .max_channel_bytes
                .checked_sub(used_bytes)
                .ok_or("the model's textures already exceed the memory budget")?,
        })
    }
}

fn is_end(layer: &psd::Layer) -> bool {
    layer.kind == LayerType::SectionDivider || layer.name == "</Layer set>" || layer.name == "</Layer group>"
}

fn blend_name(mode: BlendMode) -> &'static str {
    match mode {
        BlendMode::Multiply => "Multiply",
        BlendMode::Screen => "Screen",
        BlendMode::Overlay => "Overlay",
        BlendMode::Darken => "Darken",
        BlendMode::Lighten => "Lighten",
        BlendMode::ColorDodge => "ColorDodge",
        BlendMode::LinearDodge => "LinearDodge",
        BlendMode::ColorBurn => "ColorBurn",
        BlendMode::HardLight => "HardLight",
        BlendMode::SoftLight => "SoftLight",
        BlendMode::Difference => "Difference",
        BlendMode::Exclusion => "Exclusion",
        BlendMode::Subtract => "Subtract",
        BlendMode::LinearBurn => "LinearBurn",
        BlendMode::LinearLight => "LinearLight",
        BlendMode::Hue => "Hue",
        BlendMode::Color => "Color",
        _ => "Normal",
    }
}

/// The name a PSD blend mode goes by in the report when `blend_name` has no mode for it. Normal and pass-through are
/// not lost modes. An unknown key is shown as its four bytes when all of them are printable ASCII.
fn unmapped_psd_blend(mode: BlendMode) -> Option<String> {
    match mode {
        BlendMode::Normal | BlendMode::PassThrough => None,
        BlendMode::Unknown(key) if key.iter().all(|byte| (0x20..0x7f).contains(byte)) => {
            Some(key.iter().map(|byte| char::from(*byte)).collect())
        }
        BlendMode::Unknown(_) => Some("unknown".into()),
        mode => (blend_name(mode) == "Normal").then(|| format!("{mode:?}")),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Layer,
    Group,
    /// Closes the group opened before it.
    End,
}

/// One layer record as the importer sees it, whatever the file format.
struct SourceLayer {
    name: String,
    kind: Kind,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
    opacity: u8,
    visible: bool,
    blend: &'static str,
    /// Clipped to the nearest non-clipped layer below it in the same group (PSD only).
    clipped: bool,
    /// A group whose layers blend with what lies below the group instead of in a buffer of its own.
    pass_through: bool,
    /// The source's own name for a blend mode the engine does not have, which imports as Normal.
    unmapped_blend: Option<String>,
}

impl SourceLayer {
    fn width(&self) -> u32 {
        (i64::from(self.right) - i64::from(self.left)) as u32
    }

    fn height(&self) -> u32 {
        (i64::from(self.bottom) - i64::from(self.top)) as u32
    }
}

enum Pixels<'a> {
    Psd(&'a psd::Document<'a>),
    Kra(&'a kra::Document),
}

/// A PSD or KRA document reduced to what the importer reads. D walks PSD records from the last to the
/// first (`.reverse`) and KRA records as listed, and a KRA group is `group, children, End` where a PSD
/// group is `End, children, group` in file order, so both walks see the same shape.
struct Source<'a> {
    width: u32,
    height: u32,
    layers: Vec<SourceLayer>,
    backwards: bool,
    pixels: Pixels<'a>,
}

/// The `skippedLayers` report: the node types a Krita file has that the reader skips, with counts, and whether any
/// skipped node is visible. `None` when nothing was skipped.
fn skipped_json(skipped: &kra::Skipped) -> Option<Value> {
    if skipped.is_empty() {
        return None;
    }
    let types: Vec<Value> = skipped.types.iter().map(|(name, count)| json!({"type": name, "count": count})).collect();
    Some(json!({"visible": skipped.visible, "types": types, "other": skipped.other}))
}

/// The `blendModesNotApplied` report: how many layers and groups have a blend mode the engine does not have, and the
/// source's names for those modes, bounded like `skippedLayers`. `None` when there are none.
fn unmapped_blend_json(source: &Source) -> Option<Value> {
    let mut modes = kra::Skipped::default();
    let mut count = 0;
    let named = source.layers.iter().filter(|layer| layer.kind != Kind::End);
    for name in named.filter_map(|layer| layer.unmapped_blend.as_deref()) {
        modes.add(name, true);
        count += 1;
    }
    let shown: Vec<Value> = modes.types.iter().map(|(mode, count)| json!({"mode": mode, "count": count})).collect();
    (count > 0).then(|| json!({"count": count, "modes": shown, "other": modes.other}))
}

impl<'a> Source<'a> {
    fn from_psd(doc: &'a psd::Document<'a>) -> Self {
        let layers = doc
            .layers
            .iter()
            .map(|layer| SourceLayer {
                name: layer.name.clone(),
                kind: if is_end(layer) {
                    Kind::End
                } else if layer.kind != LayerType::Any {
                    Kind::Group
                } else {
                    Kind::Layer
                },
                left: layer.left,
                top: layer.top,
                right: layer.right,
                bottom: layer.bottom,
                opacity: layer.opacity,
                visible: layer.visible(),
                blend: blend_name(layer.blend_mode),
                clipped: layer.clipped,
                pass_through: layer.blend_mode == BlendMode::PassThrough,
                unmapped_blend: unmapped_psd_blend(layer.blend_mode),
            })
            .collect();
        Self { width: doc.width, height: doc.height, layers, backwards: true, pixels: Pixels::Psd(doc) }
    }

    fn from_kra(doc: &'a kra::Document) -> Self {
        let layers = doc
            .layers
            .iter()
            .map(|layer| SourceLayer {
                name: layer.name.clone(),
                kind: match layer.kind {
                    kra::Kind::Paint => Kind::Layer,
                    kra::Kind::Group { .. } => Kind::Group,
                    kra::Kind::End => Kind::End,
                },
                left: layer.left,
                top: layer.top,
                right: layer.right,
                bottom: layer.bottom,
                opacity: layer.opacity,
                visible: layer.visible,
                blend: layer.blend,
                clipped: false,
                pass_through: layer.pass_through,
                unmapped_blend: layer.unmapped_blend.clone(),
            })
            .collect();
        Self { width: doc.width, height: doc.height, layers, backwards: false, pixels: Pixels::Kra(doc) }
    }

    /// Indices of `layers` in the order D walks them.
    fn order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.layers.len()).collect();
        if self.backwards {
            order.reverse();
        }
        order
    }

    fn rgba(&self, index: usize) -> Result<Vec<u8>, String> {
        match &self.pixels {
            Pixels::Psd(doc) => doc.layer_rgba(index),
            Pixels::Kra(doc) => Ok(doc.layers[index].rgba.clone()),
        }
    }
}

struct Extracted {
    width: u32,
    height: u32,
    texture: Texture,
}

/// Decodes a layer to premultiplied RGBA (D `inTexPremultiply`) and PNG-encodes it. `None` for a
/// zero-area layer, which has no texture to make.
fn extract(doc: &Source, index: usize, budget: &mut Budget) -> Result<Option<Extracted>, String> {
    let layer = &doc.layers[index];
    let (width, height) = (layer.width(), layer.height());
    if width == 0 || height == 0 {
        return Ok(None);
    }
    let pixels = u64::from(width) * u64::from(height);
    if width > MAX_TEXTURE_SIDE || height > MAX_TEXTURE_SIDE || pixels > MAX_TEXTURE_PIXELS {
        return Err(format!(
            "layer {:?} is {width}x{height}, larger than the supported texture size (at most {MAX_TEXTURE_SIDE} per side and {MAX_TEXTURE_PIXELS} pixels)",
            layer.name
        ));
    }
    budget.pixels = budget
        .pixels
        .checked_sub(pixels)
        .ok_or("the layers' pixels exceed the memory budget")?;
    let mut rgba = doc.rgba(index).map_err(|error| format!("layer {:?}: {error}", layer.name))?;
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * alpha / 255) as u8;
        }
    }
    let image = image::RgbaImage::from_raw(width, height, rgba).ok_or("layer pixel buffer has the wrong size")?;
    let mut png = Cursor::new(Vec::new());
    image
        .write_to(&mut png, image::ImageFormat::Png)
        .map_err(|error| format!("could not encode layer {:?}: {error}", layer.name))?;
    let data = png.into_inner();
    budget.bytes = budget
        .bytes
        .checked_sub(data.len() as u64)
        .ok_or("the layers' textures exceed the memory budget")?;
    Ok(Some(Extracted { width, height, texture: Texture { tag: TEXTURE_PNG, data } }))
}

/// Canvas position of a layer's centre relative to the document centre (D `worldTranslation`).
fn layer_world(doc: &Source, layer: &SourceLayer, exact: bool) -> [f32; 2] {
    let centre = |start: i32, size: u32, document: u32| {
        let half = if exact { f64::from(size) / 2.0 } else { f64::from(size / 2) };
        (f64::from(start) + half - f64::from(document / 2)) as f32
    };
    [centre(layer.left, layer.width(), doc.width), centre(layer.top, layer.height(), doc.height)]
}

/// D `incCreateExPart` quad: half extents use integer division, so odd sizes lose a pixel unless `exact`.
fn quad_half(width: u32, height: u32, exact: bool) -> [f32; 2] {
    if exact {
        [width as f32 / 2.0, height as f32 / 2.0]
    } else {
        [(width / 2) as f32, (height / 2) as f32]
    }
}

fn mesh_json(half: [f32; 2]) -> Value {
    let [hw, hh] = half.map(f64::from);
    json!({
        "verts": [-hw, -hh, -hw, hh, hw, -hh, hw, hh],
        "uvs": [0.0, 0.0, 0.0, 1.0, 1.0, 0.0, 1.0, 1.0],
        "indices": [0, 1, 2, 2, 1, 3],
        "origin": [0.0, 0.0],
    })
}

fn node_json(id: u32, name: &str, kind: &str, enabled: bool, zsort: f32, trans: [f64; 3]) -> Map<String, Value> {
    let Value::Object(map) = json!({
        "uuid": id,
        "name": name,
        "type": kind,
        "enabled": enabled,
        "zsort": f64::from(zsort),
        "transform": {"trans": trans, "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
        "lockToRoot": false,
        "pinToMesh": false,
        "children": [],
    }) else {
        unreachable!()
    };
    map
}

struct PartFields<'a> {
    id: u32,
    name: &'a str,
    path: &'a str,
    half: [f32; 2],
    slot: usize,
    enabled: bool,
    zsort: f32,
    blend: &'static str,
    opacity: f32,
    trans: [f64; 3],
}

/// Bindings in the shape `node.mask.edit` writes. Not written for an empty list, as an unmasked node has none.
fn insert_masks(node: &mut Map<String, Value>, sources: &[u32]) {
    if !sources.is_empty() {
        let masks: Vec<Value> = sources.iter().map(|source| json!({"source": source, "mode": "Mask"})).collect();
        node.insert("masks".into(), Value::Array(masks));
    }
}

fn part_json(part: &PartFields) -> Value {
    let mut node = node_json(part.id, part.name, "Part", part.enabled, part.zsort, part.trans);
    node.insert("mesh".into(), mesh_json(part.half));
    node.insert("textures".into(), json!([part.slot]));
    node.insert("blend_mode".into(), json!(part.blend));
    node.insert("tint".into(), json!([1.0, 1.0, 1.0]));
    node.insert("screenTint".into(), json!([0.0, 0.0, 0.0]));
    node.insert("emissionStrength".into(), json!(1.0));
    node.insert("mask_threshold".into(), json!(0.5));
    node.insert("opacity".into(), json!(f64::from(part.opacity)));
    node.insert("psdLayerPath".into(), json!(part.path));
    Value::Object(node)
}

fn opacity_of(layer: &SourceLayer) -> f32 {
    f32::from(layer.opacity) / 255.0
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

/// A layer in the group tree D builds with `incBuildLayerLayout`.
struct Entry {
    file_index: usize,
    /// 1-based position counting every record top to bottom, group dividers included; D's
    /// `zSort = -index` is this number.
    position: u32,
    path: String,
    group: bool,
    /// `file_index` of the entry this one is clipped to: the first non-clipped sibling below it.
    clip_base: Option<usize>,
    children: Vec<Entry>,
}

/// Builds D's group tree (`incBuildLayerLayout`) walking `source` in D's order.
fn layout(source: &Source) -> Result<Vec<Entry>, String> {
    let layers = &source.layers;
    let mut out = Vec::new();
    let mut stack: Vec<Entry> = Vec::new();
    for (n, file_index) in source.order().into_iter().enumerate() {
        let layer = &layers[file_index];
        if layer.kind == Kind::End {
            let done = stack.pop().ok_or("Unexpected closing layer group")?;
            match stack.last_mut() {
                Some(parent) => parent.children.push(done),
                None => out.push(done),
            }
            continue;
        }
        let mut path = String::new();
        for open in &stack {
            path.push('/');
            path.push_str(&layers[open.file_index].name);
        }
        path.push('/');
        path.push_str(&layer.name);
        let entry = Entry {
            file_index,
            position: n as u32 + 1,
            path,
            group: layer.kind == Kind::Group,
            clip_base: None,
            children: Vec::new(),
        };
        if entry.group {
            if stack.len() >= MAX_GROUP_DEPTH {
                return Err(format!("layer groups are nested deeper than {MAX_GROUP_DEPTH} levels"));
            }
            stack.push(entry);
        } else if let Some(parent) = stack.last_mut() {
            parent.children.push(entry);
        } else {
            out.push(entry);
        }
    }
    if !stack.is_empty() {
        return Err("A layer group is never closed".into());
    }
    assign_clip_bases(&mut out, layers);
    Ok(out)
}

/// Siblings are listed top to bottom, so a clipped entry's base is the next one down that is not clipped. A run of
/// clipped entries shares one base; a run at the bottom of a group has none and is drawn unclipped.
fn assign_clip_bases(siblings: &mut [Entry], layers: &[SourceLayer]) {
    let mut base = None;
    for entry in siblings.iter_mut().rev() {
        let clipped = layers[entry.file_index].clipped;
        entry.clip_base = base.filter(|_| clipped);
        if !clipped {
            base = Some(entry.file_index);
        }
        assign_clip_bases(&mut entry.children, layers);
    }
}

fn clip_bases(siblings: &[Entry]) -> HashSet<usize> {
    let below = siblings.iter().flat_map(|entry| clip_bases(&entry.children));
    siblings.iter().filter_map(|entry| entry.clip_base).chain(below).collect()
}

enum Body {
    Group(&'static str),
    Part { half: [f32; 2], path: String, texture: Texture },
}

struct Built {
    id: u32,
    name: String,
    enabled: bool,
    zsort: f32,
    blend: &'static str,
    opacity: f32,
    /// Canvas position; locals are derived from it when writing.
    world: [f32; 2],
    body: Body,
    /// Entry this node is clipped to; resolved to `masks` once every node has an id.
    clip_base: Option<usize>,
    masks: Vec<u32>,
    children: Vec<Built>,
}

struct Importer<'a> {
    preserve_clipped_groups: bool,
    doc: &'a Source<'a>,
    keep_structure: bool,
    exact_quads: bool,
    group_type: &'static str,
    next_id: Option<u32>,
    budget: Budget,
    /// Per entry (by `file_index`), the nodes that stand for it as a mask: its own node, or for a group without a
    /// node of its own the Parts below it (the visible ones). Entries without a node have none.
    mask_sources: HashMap<usize, Vec<u32>>,
    /// Entries (by `file_index`) that are hidden; what is clipped to one is hidden too.
    hidden: HashSet<usize>,
    /// Composite-type groups open around the entry being added.
    composite_depth: usize,
    /// Entries (by `file_index`) that another entry is clipped to.
    clip_bases: HashSet<usize>,
    /// Groups with a blend mode other than Normal that became no Composite, so the mode is not applied.
    unblended_groups: Vec<(String, &'static str)>,
}

impl Importer<'_> {
    fn fresh_id(&mut self) -> Result<u32, String> {
        let id = self.next_id.ok_or("no unused UUID remains")?;
        self.next_id = id.checked_add(1).filter(|next| *next != u32::MAX);
        Ok(id)
    }

    /// Whether a layer or group below `entry` has a blend mode other than Normal (a pass-through group reads as
    /// Normal, so only what is inside it counts).
    fn blends_below(&self, entry: &Entry) -> bool {
        entry.children.iter().any(|child| self.doc.layers[child.file_index].blend != "Normal" || self.blends_below(child))
    }

    /// Appends the node(s) for `entry` to `out` in D's processing order (D then inserts every node at
    /// index 0, so the caller reverses each list).
    fn add(&mut self, out: &mut Vec<Built>, entry: &Entry) -> Result<(), String> {
        let layer = &self.doc.layers[entry.file_index];
        if entry.group {
            if !self.keep_structure {
                let first = out.len();
                for child in &entry.children {
                    self.add(out, child)?;
                }
                // The group has no node to carry its own clip, so its Parts take it.
                inherit_clip(&mut out[first..], entry.clip_base);
                if layer.blend != "Normal" {
                    self.unblended_groups.push((layer.name.clone(), layer.blend));
                }
                let sources = out[first..].iter().flat_map(part_ids).collect();
                self.mask_sources.insert(entry.file_index, sources);
                return Ok(());
            }
            // The renderer nests Composites only so deep; the groups below that become plain nodes. So does a
            // pass-through group that holds a blended layer: a Composite would blend it against the group's own
            // buffer. A group opacity has no plain-node equivalent, so such a group stays a Composite.
            // A group that others are clipped to stays one mask source: as a plain node every Part below it would be.
            let passes = layer.pass_through
                && layer.opacity == u8::MAX
                && !self.clip_bases.contains(&entry.file_index)
                && self.blends_below(entry);
            let kind = if self.composite_depth >= MAX_COMPOSITE_DEPTH || passes { "Node" } else { self.group_type };
            if kind == "Node" && layer.blend != "Normal" {
                self.unblended_groups.push((layer.name.clone(), layer.blend));
            }
            let mut node = Built {
                id: self.fresh_id()?,
                name: layer.name.clone(),
                enabled: layer.visible,
                // A plain node adds its zsort to its children's, which already hold their place in the whole stack.
                zsort: if passes { 0.0 } else { entry.position as f32 },
                blend: layer.blend,
                opacity: opacity_of(layer),
                world: [0.0, 0.0],
                body: Body::Group(kind),
                // Split flattening retains a pass-through group's clip for its enclosing clipping stack.
                clip_base: entry.clip_base.filter(|_| kind != "Node" || self.preserve_clipped_groups),
                masks: Vec::new(),
                children: Vec::new(),
            };
            self.composite_depth += usize::from(kind != "Node");
            for child in &entry.children {
                self.add(&mut node.children, child)?;
            }
            self.composite_depth -= usize::from(kind != "Node");
            if kind == "Node" && !self.preserve_clipped_groups {
                inherit_clip(&mut node.children, entry.clip_base);
            }
            node.children.reverse();
            let sources = if kind == "Node" {
                part_ids(&node).collect()
            } else {
                vec![node.id]
            };
            if !layer.visible {
                self.hidden.insert(entry.file_index);
            }
            self.mask_sources.insert(entry.file_index, sources);
            out.push(node);
            return Ok(());
        }
        let Some(extracted) = extract(self.doc, entry.file_index, &mut self.budget)? else {
            return Ok(());
        };
        let id = self.fresh_id()?;
        self.mask_sources.insert(entry.file_index, vec![id]);
        if !layer.visible {
            self.hidden.insert(entry.file_index);
        }
        out.push(Built {
            id,
            name: layer.name.clone(),
            enabled: layer.visible,
            zsort: entry.position as f32,
            blend: layer.blend,
            opacity: opacity_of(layer),
            world: layer_world(self.doc, layer, self.exact_quads),
            body: Body::Part {
                half: quad_half(extracted.width, extracted.height, self.exact_quads),
                path: entry.path.clone(),
                texture: extracted.texture,
            },
            clip_base: entry.clip_base,
            masks: Vec::new(),
            children: Vec::new(),
        });
        Ok(())
    }
}

/// The ids of the visible Parts at or below `node`.
fn part_ids(node: &Built) -> Box<dyn Iterator<Item = u32> + '_> {
    match node.body {
        _ if !node.enabled => Box::new(std::iter::empty()),
        Body::Part { .. } => Box::new(std::iter::once(node.id)),
        Body::Group(_) => Box::new(node.children.iter().flat_map(part_ids)),
    }
}

/// Gives the Parts at or below `nodes` that are not clipped inside the group the clip of the group itself.
/// ponytail: a Part already clipped inside the group keeps only that clip, because Mask bindings are a union and
/// cannot express both; add an intersecting mask mode if that case matters.
fn inherit_clip(nodes: &mut [Built], base: Option<usize>) {
    if base.is_none() {
        return;
    }
    for node in nodes {
        match node.body {
            Body::Part { .. } => node.clip_base = node.clip_base.or(base),
            Body::Group(_) => inherit_clip(&mut node.children, base),
        }
    }
}

/// Turns each node's `clip_base` into mask bindings. Photoshop clips to the base's alpha, which a Mask binding
/// reproduces (thresholded at the base's `mask_threshold`). A base with nothing to show (hidden, or an empty layer
/// without a node) hides its clipped layers, as in Photoshop; a hidden base's bindings are kept, so showing it again
/// restores them. `total` counts the bindings written, which are refused beyond the render's mask reference limit:
/// a base that expands to many Parts and many layers clipped to it multiply.
fn resolve_clipping(
    node: &mut Built,
    sources: &HashMap<usize, Vec<u32>>,
    hidden: &HashSet<usize>,
    total: &mut usize,
) -> Result<(), String> {
    if let Some(base) = node.clip_base {
        let bound = sources.get(&base).map_or(&[][..], Vec::as_slice);
        *total = total.saturating_add(bound.len());
        if *total > MAX_PACKET_MASK_REFERENCES {
            return Err(format!(
                "clipped layers need more than {MAX_PACKET_MASK_REFERENCES} mask bindings (MAX_PACKET_MASK_REFERENCES); \
                 import with a Composite or DynamicComposite layerGroupNodeType so a group is one mask source"
            ));
        }
        node.masks = bound.to_vec();
        node.enabled &= !bound.is_empty() && !hidden.contains(&base);
    }
    for child in &mut node.children {
        resolve_clipping(child, sources, hidden, total)?;
    }
    Ok(())
}

/// D `Node.centralize` for a tree of translations: every group moves to the centre of its children's
/// combined bounds (children keep their canvas position). Returns the node's combined bounds
/// `[x0, y0, x1, y1]`.
fn centralize(node: &mut Built) -> [f32; 4] {
    if let Body::Part { half, .. } = &node.body {
        let [x, y] = node.world;
        return [x - half[0], y - half[1], x + half[0], y + half[1]];
    }
    let mut bounds: Option<[f32; 4]> = None;
    for child in &mut node.children {
        let b = centralize(child);
        bounds = Some(bounds.map_or(b, |a| [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])]));
    }
    match bounds {
        Some(b) => {
            node.world = [(b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0];
            b
        }
        None => [node.world[0], node.world[1], node.world[0], node.world[1]],
    }
}

/// Writes `node` and its subtree. Texture slots follow D `populateTextureSlots`: Parts in pre-order.
fn write_node(node: Built, parent_world: [f32; 2], textures: &mut Vec<Texture>) -> Value {
    let trans = [
        f64::from(node.world[0] - parent_world[0]),
        f64::from(node.world[1] - parent_world[1]),
        0.0,
    ];
    let mut value = match node.body {
        Body::Part { half, path, texture } => {
            textures.push(texture);
            let mut part = part_json(&PartFields {
                id: node.id,
                name: &node.name,
                path: &path,
                half,
                slot: textures.len() - 1,
                enabled: node.enabled,
                zsort: node.zsort,
                blend: node.blend,
                opacity: node.opacity,
                trans,
            });
            if let Some(map) = part.as_object_mut() {
                insert_masks(map, &node.masks);
            }
            part
        }
        Body::Group(kind) => {
            let mut map = node_json(node.id, &node.name, kind, node.enabled, node.zsort, trans);
            insert_masks(&mut map, &node.masks);
            if kind != "Node" {
                if kind == "DynamicComposite" {
                    // No mesh of its own: the group follows a deformer as a rigid body, like a Composite.
                    map.insert("auto_resized".into(), json!(true));
                }
                map.insert("opacity".into(), json!(f64::from(node.opacity)));
                map.insert("blend_mode".into(), json!(node.blend));
                map.insert("tint".into(), json!([1.0, 1.0, 1.0]));
                map.insert("screenTint".into(), json!([0.0, 0.0, 0.0]));
            }
            Value::Object(map)
        }
    };
    let children: Vec<Value> = node
        .children
        .into_iter()
        .map(|child| write_node(child, node.world, textures))
        .collect();
    value["children"] = Value::Array(children);
    value
}

/// Imports a PSD as a new model (D `incImport!PSD`).
pub fn import_psd(bytes: &[u8], options: &ImportOptions, limits: &Limits) -> Result<ImportedModel, String> {
    check_options(options)?;
    let doc = psd::parse(bytes, limits)?;
    import_source(&Source::from_psd(&doc), options, limits, false)
}

pub(crate) fn import_psd_for_split(bytes: &[u8], options: &ImportOptions, limits: &Limits) -> Result<ImportedModel, String> {
    check_options(options)?;
    let doc = psd::parse(bytes, limits)?;
    import_source(&Source::from_psd(&doc), options, limits, true)
}

/// Imports a Krita document as a new model (D `incImport!KRA`).
pub fn import_kra(bytes: &[u8], options: &ImportOptions, limits: &Limits) -> Result<ImportedModel, String> {
    check_options(options)?;
    let doc = kra::parse(bytes, &kra::Limits::from(limits))?;
    let mut imported = import_source(&Source::from_kra(&doc), options, limits, false)?;
    if let Some(skipped) = skipped_json(&doc.skipped) {
        imported.report.insert("skippedLayers".into(), skipped);
    }
    Ok(imported)
}

fn check_options(options: &ImportOptions) -> Result<(), String> {
    if !GROUP_TYPES.contains(&options.group_node_type.as_str()) {
        return Err(format!("unsupported layer group node type {:?}", options.group_node_type));
    }
    if crate::valid_id(u64::from(options.first_id)).is_none() {
        return Err("first_id must be a nonzero UUID below 4294967295".into());
    }
    Ok(())
}

fn import_source(source: &Source, options: &ImportOptions, limits: &Limits, preserve_clipped_groups: bool) -> Result<ImportedModel, String> {
    let group_type = GROUP_TYPES
        .iter()
        .find(|name| **name == options.group_node_type)
        .ok_or_else(|| format!("unsupported layer group node type {:?}", options.group_node_type))?;
    let entries = layout(source)?;
    let mut importer = Importer {
        preserve_clipped_groups,
        doc: source,
        keep_structure: options.keep_structure,
        exact_quads: options.exact_quads,
        group_type,
        next_id: Some(options.first_id),
        budget: Budget::new(limits, 0)?,
        mask_sources: HashMap::new(),
        hidden: HashSet::new(),
        composite_depth: 0,
        clip_bases: clip_bases(&entries),
        unblended_groups: Vec::new(),
    };
    let root_id = importer.fresh_id()?;
    let mut children = Vec::new();
    for entry in &entries {
        importer.add(&mut children, entry)?;
    }
    children.reverse();
    let mut root = Built {
        id: root_id,
        name: "Root".into(),
        enabled: true,
        zsort: 0.0,
        blend: "Normal",
        opacity: 1.0,
        world: [0.0, 0.0],
        body: Body::Group("Node"),
        clip_base: None,
        masks: Vec::new(),
        children,
    };
    resolve_clipping(&mut root, &importer.mask_sources, &importer.hidden, &mut 0)?;
    // D centralizes each child of the root, not the root itself.
    for child in &mut root.children {
        centralize(child);
    }
    let mut textures = Vec::new();
    let nodes = write_node(root, [0.0, 0.0], &mut textures);
    let model = json!({
        "meta": {"name": "", "version": "1.0-alpha"},
        "physics": {},
        "nodes": nodes,
        "param": [],
        "automation": [],
        "animations": {},
    });
    let mut report = Map::new();
    if let Some(unmapped) = unmapped_blend_json(source) {
        report.insert("blendModesNotApplied".into(), unmapped);
    }
    // ponytail: only a Krita import says so; a PSD group in the same case is still dropped silently.
    if matches!(source.pixels, Pixels::Kra(_)) && !importer.unblended_groups.is_empty() {
        let groups = &importer.unblended_groups;
        let shown: Vec<Value> = groups
            .iter()
            .take(kra::MAX_REPORTED_GROUPS)
            .map(|(name, blend)| {
                json!({"name": name.chars().take(kra::MAX_REPORTED_NAME).collect::<String>(), "blend": blend})
            })
            .collect();
        report.insert("groupBlendNotApplied".into(), json!({"count": groups.len(), "groups": shown}));
    }
    Ok(ImportedModel { model, textures, report })
}

// ---------------------------------------------------------------------------------------------
// Merge
// ---------------------------------------------------------------------------------------------

pub(crate) fn base_name(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

/// Every Part in pre-order with the path D matches against (`layerPath`, else `/name`).
fn collect_parts(node: &Value, out: &mut Vec<(u32, String)>) {
    if node.get("type").and_then(Value::as_str) == Some("Part") {
        if let Some(id) = node.get("uuid").and_then(Value::as_u64).and_then(crate::valid_id) {
            let name = node.get("name").and_then(Value::as_str).unwrap_or("");
            let path = match node.get("psdLayerPath").and_then(Value::as_str) {
                Some(path) if !path.is_empty() => path.to_owned(),
                _ => format!("/{name}"),
            };
            out.push((id, path));
        }
    }
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        collect_parts(child, out);
    }
}

fn texture_bytes(textures: &[Texture]) -> u64 {
    textures.iter().map(|texture| texture.data.len() as u64).sum()
}

/// Matches the layers of a PSD to the Parts of `model` (the default bindings of the merge window).
pub fn merge_psd(
    model: &Value,
    textures: &[Texture],
    bytes: &[u8],
    options: &MergeOptions,
    limits: &Limits,
) -> Result<MergePlan, String> {
    check_merge_options(options)?;
    let doc = psd::parse(bytes, limits)?;
    merge_source(model, textures, &Source::from_psd(&doc), options, limits)
}

/// The same for a Krita document (D `windows/kramerge.d`).
pub fn merge_kra(
    model: &Value,
    textures: &[Texture],
    bytes: &[u8],
    options: &MergeOptions,
    limits: &Limits,
) -> Result<MergePlan, String> {
    check_merge_options(options)?;
    let doc = kra::parse(bytes, &kra::Limits::from(limits))?;
    let mut plan = merge_source(model, textures, &Source::from_kra(&doc), options, limits)?;
    plan.skipped = skipped_json(&doc.skipped);
    Ok(plan)
}

fn check_merge_options(options: &MergeOptions) -> Result<(), String> {
    if crate::valid_id(u64::from(options.first_id)).is_none() {
        return Err("first_id must be a nonzero UUID below 4294967295".into());
    }
    Ok(())
}

fn merge_source(
    model: &Value,
    textures: &[Texture],
    doc: &Source,
    options: &MergeOptions,
    limits: &Limits,
) -> Result<MergePlan, String> {
    let nodes = model.get("nodes").ok_or("model is missing nodes")?;
    let mut parts = Vec::new();
    collect_parts(nodes, &mut parts);
    let mut budget = Budget::new(limits, texture_bytes(textures))?;
    let mut segments: Vec<&str> = Vec::new();
    let mut layers = Vec::new();
    for file_index in doc.order() {
        let layer = &doc.layers[file_index];
        if layer.kind != Kind::Layer {
            if layer.kind == Kind::End {
                segments.pop();
            } else {
                segments.push(&layer.name);
            }
            continue;
        }
        let Some(extracted) = extract(doc, file_index, &mut budget)? else {
            continue;
        };
        let mut path = String::new();
        for segment in &segments {
            path.push('/');
            path.push_str(segment);
        }
        path.push('/');
        path.push_str(&layer.name);
        let by_path = parts.iter().find(|(_, candidate)| *candidate == path);
        let by_name = || parts.iter().find(|(_, candidate)| base_name(candidate) == base_name(&path));
        let (target, matched_by) = match by_path {
            Some((id, _)) => (Some(*id), Some(Match::Path)),
            None => match by_name() {
                Some((id, _)) => (Some(*id), Some(Match::Name)),
                None => (None, None),
            },
        };
        layers.push(MergeLayer {
            name: layer.name.clone(),
            path,
            left: layer.left,
            top: layer.top,
            width: extracted.width,
            height: extracted.height,
            target,
            matched_by,
            texture: extracted.texture,
            world: layer_world(doc, layer, false),
        });
    }
    let targeted: HashSet<u32> = layers.iter().filter_map(|layer| layer.target).collect();
    let unmapped_parts = parts.iter().map(|(id, _)| *id).filter(|id| !targeted.contains(id)).collect();
    Ok(MergePlan {
        document_size: [doc.width, doc.height],
        layers,
        unmapped_parts,
        options: options.clone(),
        skipped: None,
        blend_modes_not_applied: unmapped_blend_json(doc),
    })
}

fn local_matrix(node: &Value) -> Result<Matrix, String> {
    let number = |key: &str, index: usize, fallback: f64| {
        node.get("transform")
            .and_then(|transform| transform.get(key))
            .and_then(Value::as_array)
            .and_then(|values| values.get(index))
            .map_or(Some(fallback), Value::as_f64)
            .filter(|value| value.is_finite())
            .ok_or_else(|| format!("node transform.{key} must hold finite numbers"))
    };
    if number("rot", 0, 0.0)? != 0.0 || number("rot", 1, 0.0)? != 0.0 {
        return Err("a node has an unsupported X/Y rotation".into());
    }
    let (sin, cos) = number("rot", 2, 0.0)?.sin_cos();
    let (sx, sy) = (number("scale", 0, 1.0)?, number("scale", 1, 1.0)?);
    Ok([
        [cos * sx, -sin * sy, number("trans", 0, 0.0)?],
        [sin * sx, cos * sy, number("trans", 1, 0.0)?],
        [0.0, 0.0, 1.0],
    ])
}

fn multiply(a: Matrix, b: Matrix) -> Matrix {
    let mut out = [[0.0; 3]; 3];
    for (row, out_row) in out.iter_mut().enumerate() {
        for (column, cell) in out_row.iter_mut().enumerate() {
            *cell = (0..3).map(|k| a[row][k] * b[k][column]).sum();
        }
    }
    out
}

/// Inverse of an affine 2D matrix whose last row is `0 0 1`.
fn invert(m: Matrix) -> Result<Matrix, String> {
    let det = m[0][0] * m[1][1] - m[0][1] * m[1][0];
    if det.abs() <= f64::EPSILON {
        return Err("a node transform is not invertible".into());
    }
    let (a, b, c, d) = (m[1][1] / det, -m[0][1] / det, -m[1][0] / det, m[0][0] / det);
    Ok([[a, b, -(a * m[0][2] + b * m[1][2])], [c, d, -(c * m[0][2] + d * m[1][2])], [0.0, 0.0, 1.0]])
}

fn path_to<'a>(node: &'a Value, id: u32) -> Option<Vec<&'a Value>> {
    if node.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
        return Some(vec![node]);
    }
    for child in node.get("children").and_then(Value::as_array).into_iter().flatten() {
        if let Some(mut path) = path_to(child, id) {
            path.insert(0, node);
            return Some(path);
        }
    }
    None
}

/// Canvas translation `world` expressed in the space of `chain` (a root-to-node path), as D's
/// `getRelativePosition(matrix, translation(world))`: x, y, and z.
fn relative_position(chain: &[&Value], world: [f32; 2]) -> Result<[f64; 3], String> {
    let mut matrix = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
    let mut z = 0.0;
    for node in chain {
        matrix = multiply(matrix, local_matrix(node)?);
        z += node
            .get("transform")
            .and_then(|transform| transform.get("trans"))
            .and_then(Value::as_array)
            .and_then(|trans| trans.get(2))
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
            .unwrap_or(0.0);
    }
    let inverse = invert(matrix)?;
    let (x, y) = (f64::from(world[0]), f64::from(world[1]));
    Ok([
        inverse[0][0] * x + inverse[0][1] * y + inverse[0][2],
        inverse[1][0] * x + inverse[1][1] * y + inverse[1][2],
        0.0 - z,
    ])
}

fn collect_ids(value: &Value, ids: &mut HashSet<u32>) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                if matches!(key.as_str(), "uuid" | "groupUUID") {
                    if let Some(id) = entry.as_u64().and_then(|id| u32::try_from(id).ok()) {
                        ids.insert(id);
                    }
                } else {
                    collect_ids(entry, ids);
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_ids(item, ids)),
        _ => {}
    }
}

/// Rebuilds the texture slot list like D `populateTextureSlots`: slots used by nodes, numbered in
/// pre-order, unused textures dropped. The thumbnail texture is kept after them.
pub(crate) fn compact_slots(model: &mut Value, slots: Vec<Texture>) -> Result<Vec<Texture>, String> {
    fn walk(
        node: &mut Value,
        old: &[Texture],
        map: &mut Vec<Option<usize>>,
        kept: &mut Vec<Texture>,
    ) -> Result<(), String> {
        if let Some(list) = node.get_mut("textures").and_then(Value::as_array_mut) {
            for entry in list {
                let Some(slot) = entry.as_u64().filter(|slot| *slot != NO_TEXTURE) else {
                    continue;
                };
                *entry = json!(remap(slot, old, map, kept)?);
            }
        }
        for child in node.get_mut("children").and_then(Value::as_array_mut).into_iter().flatten() {
            walk(child, old, map, kept)?;
        }
        Ok(())
    }
    fn remap(slot: u64, old: &[Texture], map: &mut [Option<usize>], kept: &mut Vec<Texture>) -> Result<usize, String> {
        let index = usize::try_from(slot).ok().filter(|index| *index < old.len());
        let index = index.ok_or_else(|| format!("texture slot {slot} is not in the file"))?;
        Ok(*map[index].get_or_insert_with(|| {
            kept.push(old[index].clone());
            kept.len() - 1
        }))
    }
    let mut map = vec![None; slots.len()];
    let mut kept = Vec::new();
    if let Some(nodes) = model.get_mut("nodes") {
        walk(nodes, &slots, &mut map, &mut kept)?;
    }
    if let Some(entry) = model.get_mut("meta").and_then(|meta| meta.get_mut("thumbnailId")) {
        if let Some(slot) = entry.as_u64().filter(|slot| *slot != NO_TEXTURE) {
            *entry = json!(remap(slot, &slots, &mut map, &mut kept)?);
        }
    }
    Ok(kept)
}

/// Every texture slot a node (or the thumbnail) names must exist in the file.
pub(crate) fn check_slots(model: &Value, count: usize) -> Result<(), String> {
    fn walk(node: &Value, count: u64) -> Result<(), String> {
        for slot in node.get("textures").and_then(Value::as_array).into_iter().flatten() {
            if let Some(slot) = slot.as_u64().filter(|slot| *slot != NO_TEXTURE && *slot >= count) {
                return Err(format!("texture slot {slot} is not in the file"));
            }
        }
        node.get("children").and_then(Value::as_array).into_iter().flatten().try_for_each(|child| walk(child, count))
    }
    walk(model.get("nodes").ok_or("model is missing nodes")?, count as u64)
}

/// Applies one choice per plan layer (same order as `plan.layers`). Replacing keeps the Part's mesh
/// and updates its texture and `psdLayerPath`; adding creates a quad Part at the end of the root.
pub fn apply_merge(
    model: &Value,
    textures: &[Texture],
    plan: &MergePlan,
    choices: &[Choice],
) -> Result<Merged, String> {
    if choices.len() != plan.layers.len() {
        return Err(format!("expected {} choices, got {}", plan.layers.len(), choices.len()));
    }
    check_slots(model, textures.len())?;
    let mut model = model.clone();
    let mut slots = textures.to_vec();
    let mut used = HashSet::new();
    collect_ids(&model, &mut used);
    let mut next_id = plan.options.first_id;
    let (mut created, mut replaced) = (Vec::new(), Vec::new());
    for (layer, choice) in plan.layers.iter().zip(choices) {
        match *choice {
            Choice::Ignore => {}
            Choice::Replace(id) => {
                let (parent_position, locked) = {
                    let root = model.get("nodes").ok_or("model is missing nodes")?;
                    let chain = path_to(root, id).ok_or_else(|| format!("node {id} was not found"))?;
                    if chain[chain.len() - 1].get("type").and_then(Value::as_str) != Some("Part") {
                        return Err(format!("node {id} is not a Part"));
                    }
                    let locked = chain[chain.len() - 1].get("lockToRoot").and_then(Value::as_bool) == Some(true);
                    (relative_position(&chain[..chain.len() - 1], layer.world)?, locked)
                };
                let node = crate::nodes::find_node_mut(&mut model, id).ok_or_else(|| format!("node {id} was not found"))?;
                slots.push(layer.texture.clone());
                node["textures"] = match node.get("textures").and_then(Value::as_array) {
                    Some(list) if !list.is_empty() => {
                        let mut list = list.clone();
                        list[0] = json!(slots.len() - 1);
                        Value::Array(list)
                    }
                    _ => json!([slots.len() - 1]),
                };
                node["psdLayerPath"] = json!(layer.path);
                if plan.options.rename_mapped {
                    node["name"] = json!(layer.name);
                }
                if plan.options.retranslate_mapped {
                    if !node["transform"].is_object() {
                        node["transform"] = json!({});
                    }
                    node["transform"]["trans"] = if locked {
                        json!([f64::from(layer.world[0]), f64::from(layer.world[1]), 0.0])
                    } else {
                        json!(parent_position)
                    };
                }
                replaced.push(id);
            }
            Choice::Add => {
                while used.contains(&next_id) {
                    next_id = next_id.checked_add(1).ok_or("no unused UUID remains")?;
                }
                if crate::valid_id(u64::from(next_id)).is_none() {
                    return Err("no unused UUID remains".into());
                }
                let id = next_id;
                used.insert(id);
                let trans = {
                    let root = model.get("nodes").ok_or("model is missing nodes")?;
                    let root_id = root.get("uuid").and_then(Value::as_u64).and_then(crate::valid_id).ok_or("root has no uuid")?;
                    relative_position(&path_to(root, root_id).ok_or("root not found")?, layer.world)?
                };
                slots.push(layer.texture.clone());
                let part = part_json(&PartFields {
                    id,
                    name: &layer.name,
                    path: &layer.path,
                    half: quad_half(layer.width, layer.height, false),
                    slot: slots.len() - 1,
                    enabled: true,
                    zsort: 0.0,
                    blend: "Normal",
                    opacity: 1.0,
                    trans,
                });
                let root = model.get_mut("nodes").and_then(Value::as_object_mut).ok_or("model is missing nodes")?;
                root.entry("children")
                    .or_insert_with(|| json!([]))
                    .as_array_mut()
                    .ok_or("root children must be an array")?
                    .push(part);
                created.push(id);
            }
        }
    }
    let textures = compact_slots(&mut model, slots)?;
    Ok(Merged { model, textures, created, replaced })
}

// ---------------------------------------------------------------------------------------------
// Image files (D `incImportFolder`, `incCreatePartsFromFiles`)
// ---------------------------------------------------------------------------------------------

const MAX_IMAGE_FILE_BYTES: u64 = 256 * 1024 * 1024;

/// One PNG or TGA file as a premultiplied PNG texture (D `ShallowTexture` + `inTexPremultiply`).
#[derive(Clone, Debug)]
pub struct ImageFile {
    /// File name with its extension (`incCreatePartsFromFiles` names the Part like this).
    pub name: String,
    /// File name without the extension (`incImportFolder` names the Part like this).
    pub stem: String,
    pub width: u32,
    pub height: u32,
    pub texture: Texture,
    /// Translation of the Part from its parent (the Part is centred on it); zero for files.
    pub trans: [f64; 3],
}

/// The lower-cased extension when it is one D accepts (png, tga, jpeg, jpg).
pub fn image_extension(path: &std::path::Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "tga" | "jpeg" | "jpg").then_some(ext)
}

/// Reads and decodes one image file (PNG and TGA through `image`, baseline JPEG through [`crate::jpeg`]) as
/// premultiplied RGBA in a PNG texture (D `ShallowTexture` + `inTexPremultiply`).
pub fn read_image(path: &std::path::Path) -> Result<ImageFile, String> {
    let (name, rgba) = read_rgba(path)?;
    let stem = path.file_stem().and_then(|stem| stem.to_str()).unwrap_or_default().to_owned();
    image_from_rgba(name, stem, rgba)
}

/// Straight-alpha RGBA as a premultiplied PNG texture, named `name` (with an extension) and `stem` (without).
pub(crate) fn image_from_rgba(name: String, stem: String, mut rgba: image::RgbaImage) -> Result<ImageFile, String> {
    let fail = |message: String| format!("Could not load {name}: {message}");
    let (width, height) = rgba.dimensions();
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            *channel = (u32::from(*channel) * alpha / 255) as u8;
        }
    }
    let mut png = Cursor::new(Vec::new());
    rgba.write_to(&mut png, image::ImageFormat::Png).map_err(|error| fail(error.to_string()))?;
    Ok(ImageFile {
        name,
        stem,
        width,
        height,
        texture: Texture { tag: TEXTURE_PNG, data: png.into_inner() },
        trans: [0.0; 3],
    })
}

/// Reads and decodes one image file to straight-alpha RGBA, with the file name for messages.
pub(crate) fn read_rgba(path: &std::path::Path) -> Result<(String, image::RgbaImage), String> {
    use std::io::Read;
    let ext = image_extension(path).ok_or_else(|| format!("Invalid file type {:?}", path.extension().unwrap_or_default()))?;
    let format = match ext.as_str() {
        "png" => Some(image::ImageFormat::Png),
        "tga" => Some(image::ImageFormat::Tga),
        _ => None,
    };
    let name = path.file_name().and_then(|name| name.to_str()).ok_or("file name is not valid UTF-8")?.to_owned();
    let fail = |message: String| format!("Could not load {name}: {message}");
    let file = std::fs::File::open(path).map_err(|error| fail(error.to_string()))?;
    if !file.metadata().map_err(|error| fail(error.to_string()))?.is_file() {
        return Err(fail("not a regular file".into()));
    }
    let mut bytes = Vec::new();
    file.take(MAX_IMAGE_FILE_BYTES + 1).read_to_end(&mut bytes).map_err(|error| fail(error.to_string()))?;
    if bytes.len() as u64 > MAX_IMAGE_FILE_BYTES {
        return Err(fail("file is too large".into()));
    }
    let rgba = decode_rgba(&name, format, &bytes)?;
    Ok((name, rgba))
}

/// Refuses a picture outside the texture caps (8192 pixels a side, 16 megapixels).
pub(crate) fn check_picture_size(name: &str, width: u32, height: u32) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > MAX_TEXTURE_SIDE
        || height > MAX_TEXTURE_SIDE
        || u64::from(width) * u64::from(height) > MAX_TEXTURE_PIXELS
    {
        return Err(format!("Could not load {name}: {width}x{height} is not a supported texture size"));
    }
    Ok(())
}

/// Decodes image `bytes` (`None` is baseline JPEG) to straight-alpha RGBA within the texture caps (8192 pixels a
/// side, 16 megapixels). `name` is the only label the messages carry.
pub(crate) fn decode_rgba(name: &str, format: Option<image::ImageFormat>, bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let fail = |message: String| format!("Could not load {name}: {message}");
    match format {
        Some(format) => {
            let (width, height) = image::ImageReader::with_format(Cursor::new(bytes), format)
                .into_dimensions()
                .map_err(|error| fail(error.to_string()))?;
            check_picture_size(name, width, height)?;
            Ok(image::load_from_memory_with_format(bytes, format).map_err(|error| fail(error.to_string()))?.to_rgba8())
        }
        None => crate::jpeg::decode(bytes, MAX_TEXTURE_SIDE, MAX_TEXTURE_PIXELS).map_err(fail),
    }
}

pub(crate) fn decode_complete_png(name: &str, bytes: &[u8]) -> Result<image::RgbaImage, String> {
    let invalid = || format!("Could not load {name}: incomplete or invalid PNG chunks");
    if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") { return Err(invalid()); }
    let mut at = 8usize;
    let mut idat = false;
    let mut after_idat = false;
    let mut palette = false;
    while at < bytes.len() {
        let header = bytes.get(at..at.saturating_add(8)).ok_or_else(invalid)?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let kind = &header[4..];
        let end = at.checked_add(12).and_then(|end| end.checked_add(length)).filter(|end| *end <= bytes.len())
            .ok_or_else(invalid)?;
        if !kind.iter().all(u8::is_ascii_alphabetic) || !kind[2].is_ascii_uppercase()
            || (at == 8 && (kind != b"IHDR" || length != 13)) {
            return Err(invalid());
        }
        let crc = u32::from_be_bytes(bytes[end - 4..end].try_into().unwrap());
        if crc32fast::hash(&bytes[at + 4..end - 4]) != crc { return Err(invalid()); }
        match kind {
            b"IHDR" if at != 8 => return Err(invalid()),
            b"PLTE" => {
                if palette || idat { return Err(invalid()); }
                palette = true;
            }
            b"IDAT" => {
                if after_idat { return Err(invalid()); }
                idat = true;
            }
            b"IEND" => {
                if length != 0 || !idat || end != bytes.len() { return Err(invalid()); }
                return decode_rgba(name, Some(image::ImageFormat::Png), bytes);
            }
            b"acTL" | b"fcTL" | b"fdAT" => return Err(format!("Could not load {name}: animated PNG is not supported")),
            _ if kind[0].is_ascii_uppercase() && kind != b"IHDR" => return Err(invalid()),
            _ => {}
        }
        if idat && kind != b"IDAT" { after_idat = true; }
        at = end;
    }
    Err(invalid())
}

#[test]
fn complete_png_checks_every_chunk_and_refuses_animation() {
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(1, 1, image::Rgba([1, 2, 3, 255])))
        .write_to(&mut output, image::ImageFormat::Png).unwrap();
    let png = output.into_inner();
    assert_eq!(decode_complete_png("valid", &png).unwrap().dimensions(), (1, 1));
    for trim in [1, 4, 12] { assert!(decode_complete_png("truncated", &png[..png.len() - trim]).is_err()); }
    let mut trailing = png.clone();
    trailing.push(0);
    assert!(decode_complete_png("trailing", &trailing).is_err());
    let mut corrupt = png.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(decode_complete_png("crc", &corrupt).is_err());
    let chunk = |kind: &[u8], data: &[u8]| {
        let mut bytes = (data.len() as u32).to_be_bytes().to_vec();
        bytes.extend_from_slice(kind);
        bytes.extend_from_slice(data);
        bytes.extend_from_slice(&crc32fast::hash(&bytes[4..]).to_be_bytes());
        bytes
    };
    for (kind, data, accepted) in [(&b"tEXt"[..], &b"note\0value"[..], true),
        (&b"acTL"[..], &[0, 0, 0, 1, 0, 0, 0, 0][..], false), (&b"ABCD"[..], &[][..], false)] {
        let mut candidate = png.clone();
        candidate.splice(33..33, chunk(kind, data));
        assert_eq!(decode_complete_png("chunk", &candidate).is_ok(), accepted);
    }
}

fn image_part(id: u32, name: &str, image: &ImageFile, slot: usize, zsort: f32) -> Value {
    let mut part = part_json(&PartFields {
        id,
        name,
        path: "",
        half: quad_half(image.width, image.height, false),
        slot,
        enabled: true,
        zsort,
        blend: "Normal",
        opacity: 1.0,
        trans: image.trans,
    });
    if let Some(object) = part.as_object_mut() {
        object.remove("psdLayerPath");
    }
    part
}

/// D `incImportFolder`: a new puppet whose root holds one quad Part per image, named after the file
/// stem, `zSort = -(index / 100)`.
pub fn image_folder_model(images: &[ImageFile]) -> Result<ImportedModel, String> {
    if images.is_empty() {
        return Err("No images imported".into());
    }
    let mut model = crate::empty_model();
    let mut children = Vec::new();
    for (index, image) in images.iter().enumerate() {
        let id = u32::try_from(index + 2).map_err(|_| "too many images")?;
        children.push(image_part(id, &image.stem, image, index, 0.0 - index as f32 / 100.0));
    }
    model["nodes"]["children"] = Value::Array(children);
    let textures = images.iter().map(|image| image.texture.clone()).collect();
    Ok(ImportedModel { model, textures, report: Map::new() })
}

/// D `incCreatePartsFromFiles`: one quad Part per image appended to `parent`'s children, named after the
/// file (extension included), with new UUIDs and texture slots.
pub fn add_image_parts(model: &Value, textures: &[Texture], parent: u32, images: &[ImageFile]) -> Result<Merged, String> {
    check_slots(model, textures.len())?;
    let mut model = model.clone();
    let mut slots = textures.to_vec();
    let mut used = HashSet::new();
    collect_ids(&model, &mut used);
    let mut created = Vec::new();
    let mut next_id = 1u32;
    for image in images {
        while used.contains(&next_id) {
            next_id = next_id.checked_add(1).ok_or("no unused UUID remains")?;
        }
        if crate::valid_id(u64::from(next_id)).is_none() {
            return Err("no unused UUID remains".into());
        }
        used.insert(next_id);
        slots.push(image.texture.clone());
        let part = image_part(next_id, &image.name, image, slots.len() - 1, 0.0);
        let node = crate::nodes::find_node_mut(&mut model, parent).ok_or_else(|| format!("node {parent} was not found"))?;
        node.as_object_mut()
            .ok_or("node must be an object")?
            .entry("children")
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or("node children must be an array")?
            .push(part);
        created.push(next_id);
    }
    let textures = compact_slots(&mut model, slots)?;
    Ok(Merged { model, textures, created, replaced: Vec::new() })
}
