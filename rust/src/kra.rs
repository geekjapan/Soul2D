//! Krita (`.kra`) reader: what D gets from the `kra-d` package (`io/kra.d`).
//!
//! A KRA is a ZIP with a `mimetype`, a `maindoc.xml` that holds the layer tree and one tile file per paint
//! layer (`<image name>/layers/<filename>`). Tiles are planar BGRA, LZF compressed. Everything here is
//! bounds checked: the file is untrusted. Layers come out in `maindoc.xml` order with groups written as
//! `group, children..., End`, which is the order D's importer walks them.

use crate::psd;
use roxmltree::{Document as Xml, Node, ParsingOptions};
use std::borrow::Cow;
use std::collections::HashMap;
use std::io::Read;

const MAX_GROUP_DEPTH: usize = 48;
const MAX_TILE_SIDE: i64 = 1024;
const MAX_LINE: usize = 256;
/// Most groups, and longest name (in characters), a reply lists: the names come from the file.
pub(crate) const MAX_REPORTED_GROUPS: usize = 16;
pub(crate) const MAX_REPORTED_NAME: usize = 64;
/// Distinct node type names a [`Skipped`] report keeps.
const MAX_SKIPPED_TYPES: usize = 16;
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const METHOD_STORED: u16 = 0;
const METHOD_DEFLATE: u16 = 8;

#[derive(Clone, Debug)]
pub struct Limits {
    /// Pixels of the document, of one layer's tile bounding box (with the canvas, when the layer has a default pixel),
    /// and of all layers' boxes together.
    pub max_pixels: u64,
    pub max_layers: usize,
    pub max_entries: usize,
    /// Inflated size of one ZIP entry.
    pub max_entry_bytes: u64,
    /// Inflated size of all entries read.
    pub max_total_bytes: u64,
    pub max_xml_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pixels: 100_000_000,
            max_layers: 32_768,
            max_entries: 16_384,
            max_entry_bytes: 128 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_xml_bytes: 16 * 1024 * 1024,
        }
    }
}

impl From<&psd::Limits> for Limits {
    fn from(limits: &psd::Limits) -> Self {
        Self {
            max_pixels: limits.max_pixels,
            max_layers: limits.max_layers,
            max_total_bytes: limits.max_channel_bytes.saturating_mul(2),
            ..Self::default()
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A paint layer with pixels.
    Paint,
    /// A group opener; the group's layers follow, then [`Kind::End`].
    Group { collapsed: bool },
    /// The closing marker D's reader adds after a group's children.
    End,
}

#[derive(Clone, Debug)]
pub struct Layer {
    pub name: String,
    pub kind: Kind,
    /// Canvas rectangle of `rgba` (layer offset applied); zero area for groups and markers.
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
    pub opacity: u8,
    pub visible: bool,
    /// Engine blend name (`Normal`, `Multiply`, ...); `Normal` for anything D does not map.
    pub blend: &'static str,
    /// A group whose layers blend with what lies below the group instead of in a buffer of its own.
    pub pass_through: bool,
    /// The file's own composite op, when `engine_blend` has no mode for it and the layer imports as Normal.
    pub unmapped_blend: Option<String>,
    /// Straight-alpha RGBA8, cropped to the non-transparent pixels. Empty when the rectangle has no area.
    pub rgba: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Document {
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub layers: Vec<Layer>,
    pub skipped: Skipped,
}

/// Layer nodes the reader does not turn into layers: every `nodetype` but `paintlayer` and `grouplayer` (vector,
/// file, adjustment, ...). A paint layer without tiles and without a default pixel to show is empty, not skipped.
/// Masks (`<masks>` under a layer) are never read, so they are not counted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Skipped {
    /// `nodetype` as written (cut to [`MAX_REPORTED_NAME`] characters) and its count, in order of first appearance.
    pub types: Vec<(String, usize)>,
    /// Skipped nodes whose type did not fit in `types` ([`MAX_SKIPPED_TYPES`] names).
    pub other: usize,
    /// Whether any skipped node has `visible` other than 0 (a node without the attribute counts as visible).
    pub visible: bool,
}

impl Skipped {
    pub(crate) fn add(&mut self, node_type: &str, visible: bool) {
        self.visible |= visible;
        let name: String = node_type.chars().take(MAX_REPORTED_NAME).collect();
        if let Some((_, count)) = self.types.iter_mut().find(|(known, _)| *known == name) {
            *count += 1;
        } else if self.types.len() < MAX_SKIPPED_TYPES {
            self.types.push((name, 1));
        } else {
            self.other += 1;
        }
    }

    pub fn is_empty(&self) -> bool {
        self.types.is_empty() && self.other == 0
    }
}

/// D `incImport!KRA` blend switch over the kra-d `BlendingMode` keys. kra-d lists "lighter color" for both
/// Lighten and LighterColor, and D's switch takes Lighten. Krita's `lighten` id is the per-channel maximum, which
/// is Lighten. "lighter color" is a different operation (it keeps the whole pixel with the higher luminosity), so
/// mapping it to Lighten is an approximation kept from D.
pub fn engine_blend(key: &str) -> &'static str {
    match key {
        "multiply" => "Multiply",
        "screen" => "Screen",
        "overlay" => "Overlay",
        "darken" => "Darken",
        "lighten" | "lighter color" => "Lighten",
        "dodge" => "ColorDodge",
        "linear_dodge" => "LinearDodge",
        "burn" => "ColorBurn",
        "hard_light" => "HardLight",
        "soft_light" => "SoftLight",
        "diff" => "Difference",
        "exclusion" => "Exclusion",
        "subtract" => "Subtract",
        _ => "Normal",
    }
}

/// `op` as the report names it, when `engine_blend` has no mode for it: cut like a node type name and without control
/// characters, because it comes from the file. `normal` is Normal, not a lost mode.
fn unmapped_blend(op: &str) -> Option<String> {
    (op != "normal" && engine_blend(op) == "Normal")
        .then(|| op.chars().filter(|c| !c.is_control()).take(MAX_REPORTED_NAME).collect())
}

pub fn parse(bytes: &[u8], limits: &Limits) -> Result<Document, String> {
    let mut zip = Zip::open(bytes, limits)?;
    if zip.read("mimetype", 64)? != b"application/x-krita" {
        return Err("Invalid document: invalid mimetype.".into());
    }
    let xml = zip.read("maindoc.xml", limits.max_xml_bytes)?;
    let text = std::str::from_utf8(&xml).map_err(|_| "Invalid KRA document: maindoc.xml is not UTF-8")?;
    // roxmltree expands internal entities without a size cap; Krita never declares any.
    if text.contains("<!ENTITY") {
        return Err("Invalid KRA document: entity declarations are not supported".into());
    }
    let options = ParsingOptions { allow_dtd: true, nodes_limit: 1_000_000, ..Default::default() };
    let dom = Xml::parse_with_options(text, options).map_err(|error| format!("Invalid KRA document: {error}"))?;
    let doc = dom.root_element();
    if !doc.has_tag_name("DOC") {
        return Err("Invalid KRA document: missing DOC element".into());
    }
    let image = child(doc, "IMAGE").ok_or("Invalid KRA document: missing IMAGE element")?;
    let width = int(image, "width")?;
    let height = int(image, "height")?;
    let name = attr(image, "name")?.to_owned();
    if attr(image, "colorspacename")? != "RGBA" {
        return Err("Support for 8-bit Integer documents only.".into());
    }
    let (width, height) = match (u32::try_from(width), u32::try_from(height)) {
        (Ok(width), Ok(height)) if u64::from(width) * u64::from(height) <= limits.max_pixels => (width, height),
        _ => {
            return Err(format!(
                "Invalid KRA document: the canvas size is out of range (limit {} pixels)",
                limits.max_pixels
            ))
        }
    };
    let mut reader = Reader {
        zip,
        name: name.clone(),
        canvas: (i64::from(width), i64::from(height)),
        layers: Vec::new(),
        pixels: 0,
        skipped: Skipped::default(),
    };
    if let Some(layers) = child(image, "layers") {
        reader.walk(layers, 0)?;
    }
    Ok(Document { name, width, height, layers: reader.layers, skipped: reader.skipped })
}

fn child<'a, 'b>(node: Node<'a, 'b>, name: &str) -> Option<Node<'a, 'b>> {
    node.children().find(|candidate| candidate.is_element() && candidate.has_tag_name(name))
}

/// kra-d fails on a missing attribute, so every attribute D reads is required here too.
fn attr<'a>(node: Node<'a, '_>, name: &str) -> Result<&'a str, String> {
    node.attribute(name)
        .ok_or_else(|| format!("Invalid KRA document: missing attribute {name:?} on <{}>", node.tag_name().name()))
}

fn int(node: Node, name: &str) -> Result<i64, String> {
    attr(node, name)?
        .trim()
        .parse()
        .map_err(|_| format!("Invalid KRA document: attribute {name:?} is not an integer"))
}

struct Reader<'a> {
    zip: Zip<'a>,
    name: String,
    /// Width and height of the canvas.
    canvas: (i64, i64),
    layers: Vec<Layer>,
    pixels: u64,
    skipped: Skipped,
}

impl Reader<'_> {
    /// The default pixel of the paint layer stored as `entry`, as RGBA, unless it is transparent. Krita keeps it in
    /// `<entry>.defaultpixel` as one pixel of the layer's colour space (B, G, R, A for 8-bit RGBA). A missing entry or
    /// one of another size means transparent; the size is read from the directory, so such an entry is not inflated.
    fn default_pixel(&mut self, entry: &str) -> Result<Option<[u8; 4]>, String> {
        let name = format!("{entry}.defaultpixel");
        if self.zip.entries.get(&name).is_none_or(|found| found.size != 4) {
            return Ok(None);
        }
        let pixel = self.zip.read(&name, 4)?;
        Ok((pixel[3] != 0).then(|| [pixel[2], pixel[1], pixel[0], pixel[3]]))
    }

    fn push(&mut self, layer: Layer) -> Result<(), String> {
        if self.layers.len() >= self.zip.limits.max_layers {
            return Err(format!("the document has more than {} layers", self.zip.limits.max_layers));
        }
        self.layers.push(layer);
        Ok(())
    }

    fn walk(&mut self, layers: Node, depth: usize) -> Result<(), String> {
        if depth > MAX_GROUP_DEPTH {
            return Err(format!("layer groups are nested deeper than {MAX_GROUP_DEPTH} levels"));
        }
        for node in layers.children().filter(|node| node.is_element() && node.has_tag_name("layer")) {
            let kind = attr(node, "nodetype")?;
            if kind != "paintlayer" && kind != "grouplayer" {
                let visible = node.attribute("visible").and_then(|value| value.trim().parse::<i64>().ok());
                self.skipped.add(kind, visible != Some(0));
                continue;
            }
            let mut layer = Layer {
                name: attr(node, "name")?.to_owned(),
                kind: Kind::Paint,
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
                opacity: int(node, "opacity")?.clamp(0, 255) as u8,
                visible: int(node, "visible")? != 0,
                blend: "Normal",
                pass_through: false,
                unmapped_blend: None,
                rgba: Vec::new(),
            };
            attr(node, "uuid")?;
            let (x, y) = (int(node, "x")?, int(node, "y")?);
            if kind == "grouplayer" {
                layer.kind = Kind::Group { collapsed: int(node, "collapsed")? != 0 };
                // Unlike a paint layer's, a group's composite op was never required. Krita does not composite the
                // projection of a pass-through group, so the op of such a group has no effect.
                layer.pass_through = node.attribute("passthrough").is_some_and(|value| value.trim() == "1");
                if let (false, Some(op)) = (layer.pass_through, node.attribute("compositeop")) {
                    layer.blend = engine_blend(op);
                    layer.unmapped_blend = unmapped_blend(op);
                }
                self.push(layer)?;
                if let Some(inner) = child(node, "layers") {
                    self.walk(inner, depth + 1)?;
                }
                self.push(Layer {
                    name: String::new(),
                    kind: Kind::End,
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                    opacity: 0,
                    visible: false,
                    blend: "Normal",
                    pass_through: false,
                    unmapped_blend: None,
                    rgba: Vec::new(),
                })?;
                continue;
            }
            if attr(node, "colorspacename")? != "RGBA" {
                return Err("Support for 8-bit Integer documents only.".into());
            }
            let file = attr(node, "filename")?;
            if file.is_empty() {
                return Err("Invalid layer: filename attribute is required".into());
            }
            let op = attr(node, "compositeop")?;
            layer.blend = engine_blend(op);
            layer.unmapped_blend = unmapped_blend(op);
            let entry = format!("{}/layers/{file}", self.name);
            let data = self.zip.read(&entry, u64::MAX)?;
            let default = self.default_pixel(&entry)?;
            let limit = self.zip.limits.max_pixels;
            let tiles = read_tiles(&data, limit, default).map_err(|error| format!("layer {:?}: {error}", layer.name))?;
            // A layer without tiles but with a default pixel is all default pixel. Otherwise it is empty, and kra-d
            // drops it.
            let empty = || Tiles { data: &[], width: 1, height: 1, tiles: Vec::new(), fill: None };
            let Some(mut tiles) = tiles.or_else(|| default.map(|_| empty())) else {
                continue;
            };
            if let Some(rgba) = default {
                // The canvas in layer-local pixels, the space of the tile coordinates.
                let (Ok(x), Ok(y)) = (i32::try_from(x), i32::try_from(y)) else {
                    return Err(format!("layer {:?} is outside the supported canvas range", layer.name));
                };
                let (x, y) = (i64::from(x), i64::from(y));
                tiles.fill = Some(Fill { rect: [-x, -y, self.canvas.0 - x, self.canvas.1 - y], rgba });
            }
            self.pixels = self.pixels.saturating_add(tiles.area());
            if self.pixels > limit {
                return Err(format!(
                    "the layers' pixels exceed the memory budget (their tile boxes cover {} pixels, limit {limit})",
                    self.pixels
                ));
            }
            let (rect, rgba) = tiles.compose().map_err(|error| format!("layer {:?}: {error}", layer.name))?;
            let shift = |value: i64, offset: i64| {
                value
                    .checked_add(offset)
                    .and_then(|sum| i32::try_from(sum).ok())
                    .ok_or_else(|| format!("layer {:?} is outside the supported canvas range", layer.name))
            };
            layer.left = shift(rect[0], x)?;
            layer.top = shift(rect[1], y)?;
            layer.right = shift(rect[2], x)?;
            layer.bottom = shift(rect[3], y)?;
            layer.rgba = rgba;
            self.push(layer)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Tile data (kra-d `parseLayerData`, `extractLayer`, `cropLayer`)
// ---------------------------------------------------------------------------------------------

struct Tile {
    left: i64,
    top: i64,
    start: usize,
    end: usize,
    default_only: bool,
}

impl Tile {
    fn planar<'a>(&self, data: &'a [u8], area: usize) -> Result<Cow<'a, [u8]>, String> {
        let payload = &data[self.start..self.end];
        if payload.len() == area * 4 {
            Ok(Cow::Borrowed(payload))
        } else {
            lzf_decompress(payload, area * 4).map(Cow::Owned)
        }
    }
}

/// A layer's default pixel: every pixel that no tile covers. `rect` is the canvas, layer-local.
struct Fill {
    rect: [i64; 4],
    rgba: [u8; 4],
}

struct Tiles<'a> {
    data: &'a [u8],
    width: usize,
    height: usize,
    tiles: Vec<Tile>,
    fill: Option<Fill>,
}

impl Tiles<'_> {
    /// Pixels of the bounding box.
    fn area(&self) -> u64 {
        let (left, top, right, bottom) = self.bounds();
        ((right - left) as u64).saturating_mul((bottom - top) as u64)
    }

    /// Non-default tiles' bounding box, with the canvas when there is a default pixel.
    fn bounds(&self) -> (i64, i64, i64, i64) {
        let mut bounds = (i64::MAX, i64::MAX, i64::MIN, i64::MIN);
        if let Some(fill) = &self.fill {
            bounds = (fill.rect[0], fill.rect[1], fill.rect[2], fill.rect[3]);
        }
        for tile in self.tiles.iter().filter(|tile| !tile.default_only) {
            bounds.0 = bounds.0.min(tile.left);
            bounds.1 = bounds.1.min(tile.top);
            bounds.2 = bounds.2.max(tile.left + self.width as i64);
            bounds.3 = bounds.3.max(tile.top + self.height as i64);
        }
        if self.tiles.iter().all(|tile| tile.default_only) && self.fill.is_none() {
            return (0, 0, 0, 0);
        }
        bounds
    }

    /// Decodes the tiles into one canvas-aligned image and crops it to the non-transparent pixels.
    /// Returns `[left, top, right, bottom]` (layer-local) and the RGBA bytes; a fully transparent layer
    /// has a zero-area rectangle and no bytes (kra-d would fail there).
    fn compose(&self) -> Result<([i64; 4], Vec<u8>), String> {
        let (left, top, right, bottom) = self.bounds();
        let (columns, rows) = ((right - left) as usize, (bottom - top) as usize);
        let area = self.width * self.height;
        let mut canvas = vec![0u8; columns * rows * 4];
        if let Some(fill) = &self.fill {
            canvas.chunks_exact_mut(4).for_each(|pixel| pixel.copy_from_slice(&fill.rgba));
        }
        for tile in &self.tiles {
            let planar = tile.planar(self.data, area)?;
            let x0 = (left - tile.left).clamp(0, self.width as i64) as usize;
            let x1 = (right - tile.left).clamp(0, self.width as i64) as usize;
            let y0 = (top - tile.top).clamp(0, self.height as i64) as usize;
            let y1 = (bottom - tile.top).clamp(0, self.height as i64) as usize;
            for row in y0..y1 {
                for column in x0..x1 {
                    let source = row * self.width + column;
                    let x = (tile.left + column as i64 - left) as usize;
                    let y = (tile.top + row as i64 - top) as usize;
                    let at = (y * columns + x) * 4;
                    // Planes are B, G, R, A.
                    canvas[at] = planar[2 * area + source];
                    canvas[at + 1] = planar[area + source];
                    canvas[at + 2] = planar[source];
                    canvas[at + 3] = planar[3 * area + source];
                }
            }
        }
        let (mut min_x, mut min_y, mut max_x, mut max_y) = (usize::MAX, usize::MAX, 0, 0);
        for y in 0..rows {
            for x in 0..columns {
                if canvas[(y * columns + x) * 4 + 3] > 0 {
                    min_x = min_x.min(x);
                    min_y = min_y.min(y);
                    max_x = max_x.max(x + 1);
                    max_y = max_y.max(y + 1);
                }
            }
        }
        if max_x == 0 {
            return Ok(([left, top, left, top], Vec::new()));
        }
        let mut cropped = Vec::with_capacity((max_x - min_x) * (max_y - min_y) * 4);
        for y in min_y..max_y {
            cropped.extend_from_slice(&canvas[(y * columns + min_x) * 4..(y * columns + max_x) * 4]);
        }
        let rect = [left + min_x as i64, top + min_y as i64, left + max_x as i64, top + max_y as i64];
        Ok((rect, cropped))
    }
}

struct Lines<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Lines<'a> {
    fn line(&mut self) -> Result<&'a str, String> {
        let rest = &self.data[self.pos..];
        let end = rest
            .iter()
            .take(MAX_LINE)
            .position(|byte| *byte == b'\n')
            .ok_or("truncated or oversized tile header line")?;
        self.pos += end + 1;
        std::str::from_utf8(&rest[..end]).map_err(|_| "tile header line is not text".to_string())
    }

    /// `x,y,` at `at`: how the next tile header starts.
    fn header_at(&self, at: usize) -> bool {
        let Some(rest) = self.data.get(at..) else { return false };
        let mut commas = 0;
        for byte in rest.iter().take(24) {
            match byte {
                b'-' | b'0'..=b'9' => {}
                b',' => {
                    commas += 1;
                    if commas == 2 {
                        return true;
                    }
                }
                _ => return false,
            }
        }
        false
    }
}

fn number(text: &str, what: &str) -> Result<i64, String> {
    text.trim().parse().map_err(|_| format!("tile header {what} is not an integer"))
}

/// `None` when the file declares no tiles.
fn read_tiles(data: &[u8], max_pixels: u64, default: Option<[u8; 4]>) -> Result<Option<Tiles<'_>>, String> {
    let mut lines = Lines { data, pos: 0 };
    let mut header = HashMap::new();
    for _ in 0..5 {
        let line = lines.line()?;
        let (key, value) = line.split_once(' ').ok_or("malformed tile file header")?;
        header.insert(key, value);
    }
    let get = |key: &str| header.get(key).copied().ok_or_else(|| format!("tile file header has no {key}"));
    number(get("VERSION")?, "VERSION")?;
    let width = number(get("TILEWIDTH")?, "TILEWIDTH")?;
    let height = number(get("TILEHEIGHT")?, "TILEHEIGHT")?;
    let pixel_size = number(get("PIXELSIZE")?, "PIXELSIZE")?;
    let count = number(get("DATA")?, "DATA")?;
    if pixel_size != 4 {
        return Err("Support for 8-bit Integer documents only.".into());
    }
    if !(1..=MAX_TILE_SIDE).contains(&width) || !(1..=MAX_TILE_SIDE).contains(&height) {
        return Err(format!("tile size must be between 1 and {MAX_TILE_SIDE}"));
    }
    if count < 0 {
        return Err("tile count is negative".into());
    }
    if count == 0 {
        return Ok(None);
    }
    // Each tile costs at least a header line.
    if count as u64 > (data.len() as u64) / 8 {
        return Err("tile count exceeds the tile file size".into());
    }
    let (width, height) = (width as usize, height as usize);
    let mut tiles = Vec::new();
    for index in 0..count {
        let line = lines.line()?;
        let parts: Vec<&str> = line.split(',').collect();
        if parts.len() < 4 {
            return Err("malformed tile header".into());
        }
        let (left, top, length) = (number(parts[0], "x")?, number(parts[1], "y")?, number(parts[3], "length")?);
        if left.abs() > i64::from(i32::MAX) || top.abs() > i64::from(i32::MAX) || length < 0 {
            return Err("tile header is out of range".into());
        }
        // kra-d ignores tiles of length zero.
        if length == 0 {
            continue;
        }
        // The block is a flag byte plus LZF data. Krita counts the flag in `length`; fixtures made by
        // other writers do not, so the next header decides.
        let start = lines.pos + 1;
        let length = length as usize;
        let (end, next) = if index == count - 1 {
            let end = start.saturating_add(length).min(data.len());
            (end, data.len())
        } else if lines.header_at(lines.pos + length) {
            (lines.pos + length, lines.pos + length)
        } else if lines.header_at(lines.pos + length + 1) {
            (lines.pos + length + 1, lines.pos + length + 1)
        } else {
            return Err("tile data does not end at the next tile header".into());
        };
        if start > end || end > data.len() {
            return Err("tile data is truncated".into());
        }
        tiles.push(Tile { left, top, start, end, default_only: false });
        lines.pos = next;
    }
    if tiles.len() as u64 * (width * height) as u64 > max_pixels {
        return Err(format!("the layer's tiles exceed the memory budget (limit {max_pixels} pixels)"));
    }
    if let Some([red, green, blue, alpha]) = default {
        for tile in &mut tiles {
            let planar = tile.planar(data, width * height)?;
            tile.default_only = planar.chunks_exact(width * height).zip([blue, green, red, alpha])
                .all(|(plane, value)| plane.iter().all(|byte| *byte == value));
        }
    }
    let found = Tiles { data, width, height, tiles, fill: None };
    let (left, top, right, bottom) = found.bounds();
    if ((right - left) as u64).saturating_mul((bottom - top) as u64) > max_pixels {
        return Err(format!("the layer's tile box exceeds the memory budget (limit {max_pixels} pixels)"));
    }
    Ok(Some(found))
}

/// LZF as Krita writes it. `size` is the exact decoded length; any run that leaves it, a back reference
/// before the start, or input that ends early is an error.
pub(crate) fn lzf_decompress(input: &[u8], size: usize) -> Result<Vec<u8>, String> {
    let mut out: Vec<u8> = Vec::with_capacity(size);
    let mut at = 0;
    while at < input.len() && out.len() < size {
        let control = usize::from(input[at]);
        at += 1;
        if control < 32 {
            let run = control + 1;
            let source = input.get(at..at + run).ok_or("LZF literal run is truncated")?;
            if out.len() + run > size {
                return Err("LZF output overruns the tile".into());
            }
            out.extend_from_slice(source);
            at += run;
        } else {
            let mut length = control >> 5;
            if length == 7 {
                length += usize::from(*input.get(at).ok_or("LZF back reference is truncated")?);
                at += 1;
            }
            let low = usize::from(*input.get(at).ok_or("LZF back reference is truncated")?);
            at += 1;
            let distance = ((control & 31) << 8) + low + 1;
            length += 2;
            if distance > out.len() {
                return Err("LZF back reference points before the start".into());
            }
            if out.len() + length > size {
                return Err("LZF output overruns the tile".into());
            }
            for _ in 0..length {
                out.push(out[out.len() - distance]);
            }
        }
    }
    if out.len() != size {
        return Err("LZF data ends before the tile is complete".into());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// ZIP (central directory, stored and deflate entries)
// ---------------------------------------------------------------------------------------------

struct Entry {
    method: u16,
    crc: u32,
    start: usize,
    compressed: usize,
    size: u64,
}

struct Zip<'a> {
    bytes: &'a [u8],
    entries: HashMap<String, Entry>,
    limits: &'a Limits,
    total: u64,
}

fn u16_at(bytes: &[u8], at: usize) -> Result<u16, String> {
    let raw = bytes.get(at..at + 2).ok_or("truncated ZIP")?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}

fn u32_at(bytes: &[u8], at: usize) -> Result<u32, String> {
    let raw = bytes.get(at..at + 4).ok_or("truncated ZIP")?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}

impl<'a> Zip<'a> {
    fn open(bytes: &'a [u8], limits: &'a Limits) -> Result<Self, String> {
        let invalid = |message: &str| format!("Invalid KRA document: {message}");
        if bytes.len() < 22 {
            return Err(invalid("not a ZIP archive"));
        }
        let first = bytes.len().saturating_sub(22 + usize::from(u16::MAX));
        let eocd = (first..=bytes.len() - 22)
            .rev()
            .find(|at| {
                u32_at(bytes, *at) == Ok(EOCD_SIGNATURE)
                    && u16_at(bytes, at + 20).is_ok_and(|comment| at + 22 + usize::from(comment) == bytes.len())
            })
            .ok_or_else(|| invalid("no ZIP end record"))?;
        let (disk, directory_disk) = (u16_at(bytes, eocd + 4)?, u16_at(bytes, eocd + 6)?);
        let (on_disk, count) = (u16_at(bytes, eocd + 8)?, u16_at(bytes, eocd + 10)?);
        let (size, offset) = (u32_at(bytes, eocd + 12)?, u32_at(bytes, eocd + 16)?);
        if disk != 0 || directory_disk != 0 || on_disk != count {
            return Err(invalid("multi-disk ZIP archives are not supported"));
        }
        if count == u16::MAX || size == u32::MAX || offset == u32::MAX {
            return Err(invalid("ZIP64 archives are not supported"));
        }
        if usize::from(count) > limits.max_entries {
            return Err(invalid(&format!("the archive has more than {} entries", limits.max_entries)));
        }
        let (offset, size) = (offset as usize, size as usize);
        if offset.checked_add(size).is_none_or(|end| end > eocd) {
            return Err(invalid("the ZIP directory is out of range"));
        }
        let mut entries = HashMap::new();
        let mut at = offset;
        for _ in 0..count {
            if u32_at(bytes, at)? != CENTRAL_SIGNATURE {
                return Err(invalid("bad ZIP directory entry"));
            }
            let flags = u16_at(bytes, at + 8)?;
            let method = u16_at(bytes, at + 10)?;
            let crc = u32_at(bytes, at + 16)?;
            let compressed = u32_at(bytes, at + 20)?;
            let uncompressed = u32_at(bytes, at + 24)?;
            let (name_len, extra_len, comment_len) =
                (usize::from(u16_at(bytes, at + 28)?), usize::from(u16_at(bytes, at + 30)?), usize::from(u16_at(bytes, at + 32)?));
            let local = u32_at(bytes, at + 42)? as usize;
            let name = bytes.get(at + 46..at + 46 + name_len).ok_or_else(|| invalid("truncated ZIP directory"))?;
            at = at
                .checked_add(46 + name_len + extra_len + comment_len)
                .filter(|end| *end <= offset + size)
                .ok_or_else(|| invalid("truncated ZIP directory"))?;
            if flags & 1 != 0 {
                return Err(invalid("encrypted ZIP entries are not supported"));
            }
            if compressed == u32::MAX || uncompressed == u32::MAX {
                return Err(invalid("ZIP64 archives are not supported"));
            }
            if u32_at(bytes, local)? != LOCAL_SIGNATURE {
                return Err(invalid("bad ZIP local header"));
            }
            let data = local
                .checked_add(30 + usize::from(u16_at(bytes, local + 26)?) + usize::from(u16_at(bytes, local + 28)?))
                .ok_or_else(|| invalid("bad ZIP local header"))?;
            entries.insert(
                String::from_utf8_lossy(name).into_owned(),
                Entry { method, crc, start: data, compressed: compressed as usize, size: u64::from(uncompressed) },
            );
        }
        Ok(Self { bytes, entries, limits, total: 0 })
    }

    /// The inflated entry, checked against the directory's size and CRC. `cap` further limits the size.
    fn read(&mut self, name: &str, cap: u64) -> Result<Vec<u8>, String> {
        let entry = self.entries.get(name).ok_or_else(|| format!("Invalid document: no file '{name}'"))?;
        let entry_limit = self.limits.max_entry_bytes.min(cap);
        if entry.size > entry_limit {
            return Err(format!("'{name}' is larger than the supported size (limit {entry_limit} bytes inflated)"));
        }
        self.total = self.total.saturating_add(entry.size);
        if self.total > self.limits.max_total_bytes {
            return Err(format!(
                "the archive's contents exceed the memory budget (limit {} bytes inflated)",
                self.limits.max_total_bytes
            ));
        }
        let end = entry.start.checked_add(entry.compressed).filter(|end| *end <= self.bytes.len());
        let packed = &self.bytes[entry.start.min(self.bytes.len())..end.ok_or_else(|| format!("'{name}' is truncated"))?];
        let mut out = Vec::new();
        match entry.method {
            METHOD_STORED => out.extend_from_slice(packed),
            METHOD_DEFLATE => {
                flate2::read::DeflateDecoder::new(packed)
                    .take(entry.size + 1)
                    .read_to_end(&mut out)
                    .map_err(|error| format!("'{name}' is not valid deflate data: {error}"))?;
            }
            method => return Err(format!("'{name}' uses unsupported ZIP compression method {method}")),
        }
        if out.len() as u64 != entry.size {
            return Err(format!("'{name}' does not match its recorded size"));
        }
        if crc32fast::hash(&out) != entry.crc {
            return Err(format!("'{name}' fails its CRC check"));
        }
        Ok(out)
    }
}
