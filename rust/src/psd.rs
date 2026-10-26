//! Photoshop (.psd) reader ported from vendor/psd-d.
//!
//! Like the D library this reads version 1, 8-bit RGB documents only. Layers are kept in file order
//! (bottom to top); pixels are decoded on demand by `Document::layer_rgba`. Every input is untrusted.

use flate2::read::ZlibDecoder;
use std::io::Read;

pub const CHANNEL_R: i16 = 0;
pub const CHANNEL_G: i16 = 1;
pub const CHANNEL_B: i16 = 2;
pub const CHANNEL_TRANSPARENCY_MASK: i16 = -1;
pub const CHANNEL_LAYER_OR_VECTOR_MASK: i16 = -2;
pub const CHANNEL_LAYER_MASK: i16 = -3;

pub const LAYER_FLAG_HIDDEN: u8 = 0x02;
pub const LAYER_FLAG_PIXEL_IRRELEVANT: u8 = 0x10;

// Encoded channel data may exceed the decoded size by this much (compression headers, row tables).
const ENCODED_CHANNEL_FIXED_OVERHEAD: u64 = 64 * 1024;
const MIN_LAYER_RECORD_BYTES: u64 = 34;
// A decoded mask coexists with its duplicate and six feather-pass buffers (psdbudget.d).
const MASK_EXTRACTION_BYTES_PER_PIXEL: u64 = 8;

/// Resource caps. The defaults are the D library's constants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Maximum pixels of the document and of any one layer or mask rectangle.
    pub max_pixels: u64,
    pub max_layers: usize,
    /// Maximum decoded channels of one layer.
    pub max_layer_channels: usize,
    /// Maximum decoded (and encoded) channel bytes of one layer.
    pub max_channel_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_pixels: 100_000_000,
            max_layers: 32_768,
            max_layer_channels: 6,
            max_channel_bytes: 400_000_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayerType {
    Any,
    OpenFolder,
    ClosedFolder,
    SectionDivider,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlendMode {
    PassThrough,
    Normal,
    Dissolve,
    Darken,
    Multiply,
    ColorBurn,
    LinearBurn,
    DarkerColor,
    Lighten,
    Screen,
    ColorDodge,
    LinearDodge,
    LighterColor,
    Overlay,
    SoftLight,
    HardLight,
    VividLight,
    LinearLight,
    PinLight,
    HardMix,
    Difference,
    Exclusion,
    Subtract,
    Divide,
    Hue,
    Saturation,
    Color,
    Luminosity,
    Unknown([u8; 4]),
}

const BLEND_KEYS: [(&[u8; 4], BlendMode); 28] = [
    (b"pass", BlendMode::PassThrough),
    (b"norm", BlendMode::Normal),
    (b"diss", BlendMode::Dissolve),
    (b"dark", BlendMode::Darken),
    (b"mul ", BlendMode::Multiply),
    (b"idiv", BlendMode::ColorBurn),
    (b"lbrn", BlendMode::LinearBurn),
    (b"dkCl", BlendMode::DarkerColor),
    (b"lite", BlendMode::Lighten),
    (b"scrn", BlendMode::Screen),
    (b"div ", BlendMode::ColorDodge),
    (b"lddg", BlendMode::LinearDodge),
    (b"lgCl", BlendMode::LighterColor),
    (b"over", BlendMode::Overlay),
    (b"sLit", BlendMode::SoftLight),
    (b"hLit", BlendMode::HardLight),
    (b"vLit", BlendMode::VividLight),
    (b"lLit", BlendMode::LinearLight),
    (b"pLit", BlendMode::PinLight),
    (b"hMix", BlendMode::HardMix),
    (b"diff", BlendMode::Difference),
    (b"smud", BlendMode::Exclusion),
    (b"fsub", BlendMode::Subtract),
    (b"fdiv", BlendMode::Divide),
    (b"hue ", BlendMode::Hue),
    (b"sat ", BlendMode::Saturation),
    (b"colr", BlendMode::Color),
    (b"lum ", BlendMode::Luminosity),
];

impl BlendMode {
    pub fn from_key(key: [u8; 4]) -> Self {
        BLEND_KEYS
            .iter()
            .find(|(candidate, _)| **candidate == key)
            .map(|(_, mode)| *mode)
            .unwrap_or(BlendMode::Unknown(key))
    }

    pub fn key(self) -> [u8; 4] {
        match self {
            BlendMode::Unknown(key) => key,
            mode => *BLEND_KEYS.iter().find(|(_, candidate)| *candidate == mode).unwrap().0,
        }
    }
}

/// Layer or vector mask rectangle and settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mask {
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    pub feather: f64,
    pub density: u8,
    pub default_color: u8,
    pub disabled: bool,
    pub invert: bool,
    pub position_relative_to_layer: bool,
}

impl Mask {
    fn width(&self) -> i64 {
        i64::from(self.right) - i64::from(self.left)
    }

    fn height(&self) -> i64 {
        i64::from(self.bottom) - i64::from(self.top)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelInfo {
    /// 0/1/2 are R/G/B, -1 transparency, -2 user or vector mask, -3 layer mask.
    pub id: i16,
    /// Absolute offset of the channel data (compression header included).
    pub offset: usize,
    pub length: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    /// The Unicode (`luni`) name when present and non-empty, else the Pascal name.
    pub name: String,
    pub top: i32,
    pub left: i32,
    pub bottom: i32,
    pub right: i32,
    pub blend_mode: BlendMode,
    pub opacity: u8,
    /// True when the layer is clipped to the layer below it. D's `Layer.clipping` is the negation.
    pub clipped: bool,
    pub flags: u8,
    pub kind: LayerType,
    pub channels: Vec<ChannelInfo>,
    pub layer_mask: Option<Mask>,
    pub vector_mask: Option<Mask>,
}

impl Layer {
    pub fn width(&self) -> u32 {
        (i64::from(self.right) - i64::from(self.left)) as u32
    }

    pub fn height(&self) -> u32 {
        (i64::from(self.bottom) - i64::from(self.top)) as u32
    }

    /// Photoshop stores the "invisible" bit in the flag that the specification calls "visible".
    pub fn visible(&self) -> bool {
        self.flags & LAYER_FLAG_HIDDEN == 0
    }

    pub fn is_group(&self) -> bool {
        matches!(self.kind, LayerType::OpenFolder | LayerType::ClosedFolder)
    }

    /// An ordinary layer with a non-empty rectangle whose pixels matter.
    pub fn has_pixel_data(&self) -> bool {
        self.kind == LayerType::Any
            && self.flags & LAYER_FLAG_PIXEL_IRRELEVANT == 0
            && self.width() > 0
            && self.height() > 0
    }
}

#[derive(Debug)]
pub struct Document<'a> {
    pub width: u32,
    pub height: u32,
    /// Channel count of the document header (including alpha channels).
    pub channels: u16,
    /// True when the layer count was negative: the first alpha channel is the merged transparency.
    pub has_transparency_mask: bool,
    /// Layers in file order, bottom to top. Group openers (`OpenFolder`/`ClosedFolder`) follow
    /// their children; the matching `SectionDivider` precedes them.
    pub layers: Vec<Layer>,
    data: &'a [u8],
    limits: Limits,
}

impl Document<'_> {
    /// Decodes one layer to RGBA8 (`width * height * 4` bytes; empty for a zero-area layer).
    /// Masks are folded into alpha. Channels missing from the file read as 0 (alpha as 255).
    pub fn layer_rgba(&self, index: usize) -> Result<Vec<u8>, String> {
        let layer = self.layers.get(index).ok_or("PSD layer index out of range")?;
        extract_layer(self.data, layer, &self.limits)
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    end: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            pos: 0,
            end: data.len(),
        }
    }

    fn remaining(&self) -> usize {
        self.end - self.pos
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], String> {
        if len > self.remaining() {
            return Err("Truncated PSD: unexpected end of data".into());
        }
        let slice = &self.data[self.pos..self.pos + len];
        self.pos += len;
        Ok(slice)
    }

    fn sub(&mut self, len: usize) -> Result<Reader<'a>, String> {
        let start = self.pos;
        self.take(len)?;
        Ok(Reader {
            data: self.data,
            pos: start,
            end: start + len,
        })
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, String> {
        let b = self.take(2)?;
        Ok(u16::from_be_bytes([b[0], b[1]]))
    }

    fn i16(&mut self) -> Result<i16, String> {
        Ok(self.u16()? as i16)
    }

    fn u32(&mut self) -> Result<u32, String> {
        let b = self.take(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn i32(&mut self) -> Result<i32, String> {
        Ok(self.u32()? as i32)
    }

    fn f64(&mut self) -> Result<f64, String> {
        let b = self.take(8)?;
        Ok(f64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]))
    }

    fn tag(&mut self) -> Result<[u8; 4], String> {
        let b = self.take(4)?;
        Ok([b[0], b[1], b[2], b[3]])
    }
}

fn valid_image_dimensions(width: i64, height: i64, max_pixels: u64) -> bool {
    if width < 0 || height < 0 {
        return false;
    }
    let (width, height) = (width as u64, height as u64);
    width == 0 || height == 0 || width <= max_pixels / height
}

pub fn parse<'a>(bytes: &'a [u8], limits: &Limits) -> Result<Document<'a>, String> {
    let mut r = Reader::new(bytes);
    if &r.tag()? != b"8BPS" {
        return Err("Invalid file, must be a Photoshop PSD file. PSB's are not supported!".into());
    }
    if r.u16()? != 1 {
        return Err("Version does not match 1.".into());
    }
    if r.take(6)? != [0u8; 6] {
        return Err("Unexpected reserve bytes, file may be corrupted.".into());
    }
    let channels = r.u16()?;
    let height = r.u32()?;
    let width = r.u32()?;
    if height == 0
        || width == 0
        || height > i32::MAX as u32
        || width > i32::MAX as u32
        || !valid_image_dimensions(i64::from(width), i64::from(height), limits.max_pixels)
    {
        return Err("PSD document dimensions exceed the supported decoded image size".into());
    }
    let depth = r.u16()?;
    if depth != 8 {
        return Err(format!("Only 8-bit PSD channels are supported (found {depth}-bit)"));
    }
    let mode = r.u16()?;
    if mode != 3 {
        return Err(format!("Only RGB PSD documents are supported (found color mode {mode})"));
    }
    let color_mode_len = r.u32()? as usize;
    r.take(color_mode_len)?;
    // Image resources are skipped, as in D (its resource parser is dead code).
    let resources_len = r.u32()? as usize;
    r.take(resources_len)?;
    let section_len = r.u32()? as usize;
    let (layers, has_transparency_mask) = if section_len == 0 {
        (Vec::new(), false)
    } else {
        let set = parse_layer_mask_section(&mut r.sub(section_len)?, limits)?;
        (set.layers, set.has_transparency_mask)
    };
    Ok(Document {
        width,
        height,
        channels,
        has_transparency_mask,
        layers,
        data: bytes,
        limits: limits.clone(),
    })
}

struct LayerSet {
    layers: Vec<Layer>,
    has_transparency_mask: bool,
}

fn parse_layer_mask_section(r: &mut Reader<'_>, limits: &Limits) -> Result<LayerSet, String> {
    let info_len = r.u32()? as usize;
    let mut set = LayerSet {
        layers: Vec::new(),
        has_transparency_mask: false,
    };
    if info_len != 0 {
        set = parse_layer_info(&mut r.sub(info_len)?, limits)?;
    }
    if r.remaining() == 0 {
        return Ok(set);
    }
    let global_len = r.u32()? as usize;
    if global_len != 0 {
        // Overlay colour space, colour, opacity and kind: 13 bytes that D reads and discards.
        if global_len < 13 {
            return Err("PSD global layer mask info is truncated".into());
        }
        r.take(global_len)?;
    }
    // Fewer than 12 trailing bytes cannot hold a block header; treat them as padding.
    while r.remaining() >= 12 {
        if &r.tag()? != b"8BIM" {
            return Err("Additional Layer Information section seems to be corrupt, \
                signature does not match \"8BIM\"."
                .into());
        }
        let key = r.tag()?;
        let length = r.u32()? as usize;
        let padded = length.checked_add(3).ok_or("PSD additional info length overflow")? & !3;
        let take = if padded <= r.remaining() {
            padded
        } else if length <= r.remaining() {
            r.remaining()
        } else {
            return Err("Additional Layer Information block exceeds its containing section".into());
        };
        let mut block = r.sub(take)?;
        if &key == b"Lr16" || &key == b"Lr32" {
            set = parse_layer_info(&mut block, limits)?;
        }
    }
    Ok(set)
}

fn parse_layer_info(r: &mut Reader<'_>, limits: &Limits) -> Result<LayerSet, String> {
    let layer_length = r.remaining() as u64;
    let encoded = r.i16()?;
    let count = u64::from(encoded.unsigned_abs());
    if count > (layer_length - 2) / MIN_LAYER_RECORD_BYTES || count > limits.max_layers as u64 {
        return Err("PSD layer count exceeds the available layer info section".into());
    }
    let mut layers = Vec::with_capacity(count as usize);
    for _ in 0..count {
        layers.push(read_layer_record(r, limits)?);
    }
    // Channel payloads follow all records, layer by layer; only their offsets are kept.
    for layer in &mut layers {
        for channel in &mut layer.channels {
            channel.offset = r.pos;
            r.take(channel.length as usize)
                .map_err(|_| "PSD channel data exceeds the layer info section".to_string())?;
        }
    }
    Ok(LayerSet {
        layers,
        has_transparency_mask: encoded < 0,
    })
}

fn read_layer_record(r: &mut Reader<'_>, limits: &Limits) -> Result<Layer, String> {
    let top = r.i32()?;
    let left = r.i32()?;
    let bottom = r.i32()?;
    let right = r.i32()?;
    let width = i64::from(right) - i64::from(left);
    let height = i64::from(bottom) - i64::from(top);
    if !valid_image_dimensions(width, height, limits.max_pixels) {
        return Err(format!(
            "Invalid or excessively large PSD layer bounds: ({left}, {top})-({right}, {bottom})"
        ));
    }
    let channel_count = r.u16()? as usize;
    if channel_count * 6 > r.remaining() {
        return Err("Truncated PSD: layer channel table exceeds the layer info section".into());
    }
    let mut channels = Vec::with_capacity(channel_count);
    for _ in 0..channel_count {
        channels.push(ChannelInfo {
            id: r.i16()?,
            offset: 0,
            length: r.u32()?,
        });
    }
    if &r.tag()? != b"8BIM" {
        return Err("Layer mask info section seems to be corrupt, signature does not match \"8BIM\".".into());
    }
    let blend_mode = BlendMode::from_key(r.tag()?);
    let opacity = r.u8()?;
    let clipped = r.u8()? != 0;
    let flags = r.u8()?;
    r.u8()?;
    let extra_len = r.u32()? as usize;
    let mut extra = r.sub(extra_len)?;

    let mask_len = extra.u32()? as usize;
    let (layer_mask, vector_mask) = read_masks(&mut extra.sub(mask_len)?)?;
    let blending_ranges_len = extra.u32()? as usize;
    extra.take(blending_ranges_len)?;

    // Pascal string padded so that length byte plus text is a multiple of 4.
    let name_len = extra.u8()? as usize;
    let name_bytes = extra.take(name_len)?;
    extra.take(((name_len + 1 + 3) & !3) - name_len - 1)?;
    let mut name = String::from_utf8_lossy(name_bytes).into_owned();

    let mut kind = LayerType::Any;
    while extra.remaining() > 0 {
        if extra.remaining() < 12 {
            return Err("Additional Layer Information block header is truncated".into());
        }
        if &extra.tag()? != b"8BIM" {
            return Err("Additional Layer Information section seems to be corrupt, \
                signature does not match \"8BIM\"."
                .into());
        }
        let key = extra.tag()?;
        let length = extra.u32()? as usize;
        let padded = length.checked_add(length & 1).ok_or("PSD additional info length overflow")?;
        let take = if padded <= extra.remaining() {
            padded
        } else if length <= extra.remaining() {
            extra.remaining()
        } else {
            return Err("Additional Layer Information block exceeds its containing section".into());
        };
        let payload = &extra.take(take)?[..length];
        match &key {
            b"lsct" => {
                if payload.len() < 4 {
                    return Err("PSD section divider block is truncated".into());
                }
                kind = match u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) {
                    0 => LayerType::Any,
                    1 => LayerType::OpenFolder,
                    2 => LayerType::ClosedFolder,
                    3 => LayerType::SectionDivider,
                    other => return Err(format!("Unsupported PSD section divider type {other}")),
                };
            }
            b"luni" => {
                if payload.len() < 4 {
                    return Err("PSD Unicode layer name block is truncated".into());
                }
                let chars = u32::from_be_bytes([payload[0], payload[1], payload[2], payload[3]]) as usize;
                if chars > (payload.len() - 4) / 2 {
                    return Err("PSD Unicode layer name exceeds its additional-info block".into());
                }
                let units: Vec<u16> = payload[4..4 + chars * 2]
                    .chunks_exact(2)
                    .map(|unit| u16::from_be_bytes([unit[0], unit[1]]))
                    .collect();
                let mut unicode = String::from_utf16_lossy(&units);
                // Some exporters add a terminating NUL.
                if unicode.ends_with('\0') {
                    unicode.pop();
                }
                // An empty luni block keeps the Pascal name.
                if !unicode.is_empty() {
                    name = unicode;
                }
            }
            _ => {}
        }
    }
    Ok(Layer {
        name,
        top,
        left,
        bottom,
        right,
        blend_mode,
        opacity,
        clipped,
        flags,
        kind,
        channels,
        layer_mask,
        vector_mask,
    })
}

struct RawMask {
    rect: [i32; 4],
    default_color: u8,
    flags: u8,
}

fn read_masks(r: &mut Reader<'_>) -> Result<(Option<Mask>, Option<Mask>), String> {
    if r.remaining() == 0 {
        return Ok((None, None));
    }
    let total = r.remaining();
    let first = RawMask {
        rect: [r.i32()?, r.i32()?, r.i32()?, r.i32()?],
        default_color: r.u8()?,
        flags: r.u8()?,
    };
    // Order: layer density, layer feather, vector density, vector feather.
    let mut density = [255u8; 2];
    let mut feather = [0f64; 2];
    let mut has_parameters = first.flags & 0x10 != 0;
    if has_parameters && total <= 28 {
        read_mask_parameters(r, &mut density, &mut feather)?;
    }
    // Enough data left means a second mask; the first one then described the vector mask.
    let second = if r.remaining() >= 18 {
        let flags = r.u8()?;
        let default_color = r.u8()?;
        let rect = [r.i32()?, r.i32()?, r.i32()?, r.i32()?];
        has_parameters |= flags & 0x10 != 0;
        if has_parameters {
            read_mask_parameters(r, &mut density, &mut feather)?;
        }
        Some(RawMask {
            rect,
            default_color,
            flags,
        })
    } else {
        None
    };
    let mut raws = vec![first];
    raws.extend(second);

    let (mut layer_mask, mut vector_mask) = (None, None);
    for raw in raws {
        let vector = raw.flags & 0x08 != 0;
        let slot = usize::from(vector);
        let mask = Mask {
            top: raw.rect[0],
            left: raw.rect[1],
            bottom: raw.rect[2],
            right: raw.rect[3],
            feather: feather[slot],
            density: density[slot],
            default_color: raw.default_color,
            disabled: raw.flags & 0x02 != 0,
            invert: raw.flags & 0x04 != 0,
            position_relative_to_layer: raw.flags & 0x01 != 0,
        };
        let target = if vector { &mut vector_mask } else { &mut layer_mask };
        if target.replace(mask).is_some() {
            return Err(format!(
                "A {} mask already exists.",
                if vector { "vector" } else { "layer" }
            ));
        }
    }
    Ok((layer_mask, vector_mask))
}

fn read_mask_parameters(r: &mut Reader<'_>, density: &mut [u8; 2], feather: &mut [f64; 2]) -> Result<(), String> {
    let flags = r.u8()?;
    if flags & 0x01 != 0 {
        density[0] = r.u8()?;
    }
    if flags & 0x02 != 0 {
        feather[0] = r.f64()?;
    }
    if flags & 0x04 != 0 {
        density[1] = r.u8()?;
    }
    if flags & 0x08 != 0 {
        feather[1] = r.f64()?;
    }
    Ok(())
}

/// Per-layer group path, visibility and cumulative opacity.
#[derive(Clone, Debug, PartialEq)]
pub struct GroupState {
    pub path: String,
    pub visible: bool,
    pub opacity: f32,
}

/// Port of `ngPsdLayerGroupStates`. `layers` must be in top-to-bottom order (folder, children,
/// divider), which is the reverse of file order.
pub fn group_states(layers: &[Layer]) -> Vec<GroupState> {
    let mut states = Vec::with_capacity(layers.len());
    let mut stack: Vec<(&str, bool, f32)> = Vec::new();
    for layer in layers {
        let mut state = GroupState {
            path: String::new(),
            visible: true,
            opacity: 1.0,
        };
        match layer.kind {
            LayerType::OpenFolder | LayerType::ClosedFolder => {
                let (_, parent_visible, parent_opacity) = stack.last().copied().unwrap_or(("", true, 1.0));
                stack.push((
                    &layer.name,
                    parent_visible && layer.visible(),
                    parent_opacity * f32::from(layer.opacity) / 255.0,
                ));
            }
            LayerType::SectionDivider => {
                stack.pop();
            }
            LayerType::Any => {
                if let Some(&(_, visible, opacity)) = stack.last() {
                    state.path = stack.iter().fold(String::new(), |path, (name, ..)| path + "/" + name);
                    state.visible = visible;
                    state.opacity = opacity;
                }
            }
        }
        states.push(state);
    }
    states
}

/// PackBits decode of one scanline; the stream must fill `destination` exactly.
pub fn decode_rle(source: &[u8], destination: &mut [u8]) -> Result<(), String> {
    let (mut src, mut dst) = (0usize, 0usize);
    while src < source.len() {
        let tag = source[src];
        src += 1;
        if tag == 0x80 {
            continue;
        }
        if tag > 0x80 {
            let count = 257 - usize::from(tag);
            let value = *source
                .get(src)
                .ok_or("Truncated PSD RLE stream: missing replicated byte")?;
            if count > destination.len() - dst {
                return Err("Invalid PSD RLE stream: replicated run exceeds destination".into());
            }
            destination[dst..dst + count].fill(value);
            src += 1;
            dst += count;
        } else {
            let count = usize::from(tag) + 1;
            if count > source.len() - src {
                return Err("Truncated PSD RLE stream: literal run exceeds source".into());
            }
            if count > destination.len() - dst {
                return Err("Invalid PSD RLE stream: literal run exceeds destination".into());
            }
            destination[dst..dst + count].copy_from_slice(&source[src..src + count]);
            src += count;
            dst += count;
        }
    }
    if dst != destination.len() {
        return Err("Truncated PSD RLE stream: scanline is shorter than its destination".into());
    }
    Ok(())
}

/// Inflates a ZIP channel (optionally with per-row horizontal prediction) to exactly `width * height` bytes.
pub fn decode_zip(source: &[u8], width: u32, height: u32, prediction: bool) -> Result<Vec<u8>, String> {
    let decoded_len = usize::try_from(u64::from(width) * u64::from(height))
        .map_err(|_| "ZIP-compressed PSD channel is too large".to_string())?;
    let mut data = Vec::new();
    // One byte beyond the declared size detects over-long streams without inflating a bomb.
    ZlibDecoder::new(source)
        .take(decoded_len as u64 + 1)
        .read_to_end(&mut data)
        .map_err(|_| "Invalid ZIP-compressed PSD channel data".to_string())?;
    if data.len() > decoded_len {
        return Err("ZIP-compressed PSD channel exceeds its declared dimensions".into());
    }
    if data.len() != decoded_len {
        return Err("Invalid ZIP-compressed PSD channel length".into());
    }
    if prediction && width > 0 {
        for row in data.chunks_exact_mut(width as usize) {
            for x in 1..row.len() {
                row[x] = row[x].wrapping_add(row[x - 1]);
            }
        }
    }
    Ok(data)
}

/// Applies disabled, invert and density settings to one mask sample.
pub fn apply_mask_settings(mask: u8, density: u8, disabled: bool, invert: bool) -> u8 {
    if disabled {
        return 255;
    }
    let mask = if invert { 255 - mask } else { mask };
    // Density 0 disables the mask effect; 255 applies it fully.
    (255 - u32::from(density) + (u32::from(mask) * u32::from(density) + 127) / 255) as u8
}

/// Three-pass box blur approximating a Gaussian feather. Returns a copy when nothing applies.
pub fn apply_mask_feather(data: &[u8], width: usize, height: usize, feather: f64, default_color: u8) -> Vec<u8> {
    if width == 0
        || height == 0
        || width.checked_mul(height) != Some(data.len())
        || !feather.is_finite()
        || feather <= 0.0
    {
        return data.to_vec();
    }
    let radius_value = (((4.0 * feather * feather + 1.0).sqrt() - 1.0) / 2.0).round();
    let radius = if radius_value < 1.0 {
        1
    } else if radius_value > 1_000_000.0 {
        1_000_000
    } else {
        radius_value as i64
    };
    let mut result = data.to_vec();
    for _ in 0..3 {
        result = box_blur(&result, width, height, radius, true, default_color);
        result = box_blur(&result, width, height, radius, false, default_color);
    }
    result
}

fn box_blur(data: &[u8], width: usize, height: usize, radius: i64, horizontal: bool, default_color: u8) -> Vec<u8> {
    let (line_length, line_count) = if horizontal { (width, height) } else { (height, width) };
    let window = radius as u64 * 2 + 1;
    let index_of = |line: usize, position: usize| {
        if horizontal {
            line * width + position
        } else {
            position * width + line
        }
    };
    let mut result = vec![0u8; data.len()];
    let mut prefix = vec![0u64; line_length + 1];
    for line in 0..line_count {
        for position in 0..line_length {
            prefix[position + 1] = prefix[position] + u64::from(data[index_of(line, position)]);
        }
        for position in 0..line_length {
            let start = (position as i64 - radius).clamp(0, line_length as i64) as usize;
            let end = (position as i64 + radius + 1).clamp(0, line_length as i64) as usize;
            let inside = (end - start) as u64;
            let sum = prefix[end] - prefix[start] + (window - inside) * u64::from(default_color);
            result[index_of(line, position)] = ((sum + window / 2) / window) as u8;
        }
    }
    result
}

fn mask_for_channel(layer: &Layer, id: i16) -> Option<&Mask> {
    match id {
        CHANNEL_LAYER_MASK => layer.layer_mask.as_ref(),
        // -2 is the vector mask when both exist, otherwise whichever user mask is present.
        CHANNEL_LAYER_OR_VECTOR_MASK => layer.vector_mask.as_ref().or(layer.layer_mask.as_ref()),
        _ => None,
    }
}

fn decoded_channel(id: i16) -> bool {
    matches!(
        id,
        CHANNEL_R
            | CHANNEL_G
            | CHANNEL_B
            | CHANNEL_TRANSPARENCY_MASK
            | CHANNEL_LAYER_OR_VECTOR_MASK
            | CHANNEL_LAYER_MASK
    )
}

fn channel_dimensions(layer: &Layer, id: i16, limits: &Limits) -> Result<(usize, usize), String> {
    let (mut width, mut height) = (i64::from(layer.width()), i64::from(layer.height()));
    if let Some(mask) = mask_for_channel(layer, id) {
        let (mask_width, mask_height) = (mask.width(), mask.height());
        if !valid_image_dimensions(mask_width, mask_height, limits.max_pixels) {
            return Err("Invalid or excessively large PSD mask bounds".into());
        }
        if mask_width > 0 && mask_height > 0 {
            width = mask_width;
            height = mask_height;
        }
    }
    Ok((width as usize, height as usize))
}

fn valid_encoded_channel_length(encoded: u64, decoded: u64, row_table: u64, max: u64) -> bool {
    if encoded > max || decoded > max || row_table > max - decoded {
        return false;
    }
    let base = decoded + row_table;
    let proportional = decoded / 16;
    if proportional > max - base {
        return false;
    }
    let base = base + proportional;
    let supported = if base > max.saturating_sub(ENCODED_CHANNEL_FIXED_OVERHEAD) {
        max
    } else {
        base + ENCODED_CHANNEL_FIXED_OVERHEAD
    };
    encoded <= supported
}

fn decode_channel(data: &[u8], channel: &ChannelInfo, width: usize, height: usize) -> Result<Vec<u8>, String> {
    let payload = channel
        .offset
        .checked_add(channel.length as usize)
        .and_then(|end| data.get(channel.offset..end))
        .ok_or("Truncated PSD channel payload")?;
    let compression = u16::from_be_bytes([payload[0], payload[1]]);
    let encoded = &payload[2..];
    let decoded_len = width * height;
    match compression {
        0 => {
            if encoded.len() < decoded_len {
                return Err("Truncated raw PSD channel data".into());
            }
            Ok(encoded[..decoded_len].to_vec())
        }
        1 => {
            let table_len = height * 2;
            if table_len > encoded.len() {
                return Err("Truncated PSD RLE channel data".into());
            }
            let (table, rle) = encoded.split_at(table_len);
            let sizes: Vec<usize> = table
                .chunks_exact(2)
                .map(|size| usize::from(u16::from_be_bytes([size[0], size[1]])))
                .collect();
            let total: usize = sizes.iter().sum();
            if total > rle.len() {
                return Err("Truncated PSD RLE channel data".into());
            }
            if total == 0 {
                if decoded_len != 0 {
                    return Err("Truncated PSD RLE channel data".into());
                }
                return Ok(Vec::new());
            }
            // Rows are decoded independently so a run cannot spill into the next scanline. The
            // output grows row by row, so a tiny file cannot force a large allocation up front.
            let mut out = Vec::new();
            let mut offset = 0;
            for size in sizes {
                let start = out.len();
                out.resize(start + width, 0);
                decode_rle(&rle[offset..offset + size], &mut out[start..])?;
                offset += size;
            }
            Ok(out)
        }
        2 | 3 => decode_zip(encoded, width as u32, height as u32, compression == 3),
        other => Err(format!("Unsupported PSD channel compression type: {other}")),
    }
}

fn extract_layer(data: &[u8], layer: &Layer, limits: &Limits) -> Result<Vec<u8>, String> {
    let (layer_width, layer_height) = (layer.width() as usize, layer.height() as usize);
    if layer_width == 0 || layer_height == 0 {
        return Ok(Vec::new());
    }
    let max = limits.max_channel_bytes;
    // The RGBA output is allocated even when every channel is missing, so cap it up front.
    if (layer_width as u64).saturating_mul(layer_height as u64).saturating_mul(4) > max {
        return Err("PSD layer decoded channel data exceeds the supported memory budget".into());
    }
    let mut decoded: Vec<(i16, Vec<u8>)> = Vec::new();
    let mut decoded_bytes = 0u64;
    for channel in &layer.channels {
        if !decoded_channel(channel.id) {
            continue;
        }
        let (width, height) = channel_dimensions(layer, channel.id, limits)?;
        if channel.length < 2 {
            return Err("Invalid PSD channel length: missing compression header".into());
        }
        let encoded_len = u64::from(channel.length) - 2;
        let decoded_len = width as u64 * height as u64;
        if decoded.len() >= limits.max_layer_channels
            || decoded_bytes > max
            || decoded_len > max - decoded_bytes
        {
            return Err("PSD layer decoded channel data exceeds the supported memory budget".into());
        }
        decoded_bytes += decoded_len;
        let compression = data
            .get(channel.offset..channel.offset.saturating_add(2))
            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]));
        let row_table = if compression == Some(1) { height as u64 * 2 } else { 0 };
        if !valid_encoded_channel_length(encoded_len, decoded_len, row_table, max) {
            return Err("PSD layer encoded channel data exceeds the supported memory budget".into());
        }
        decoded.push((channel.id, decode_channel(data, channel, width, height)?));
    }
    // First channel of each id with a payload wins, as D's findChannel does.
    let find = |id: i16| {
        layer
            .channels
            .iter()
            .position(|channel| channel.length > 0 && channel.id == id)
            .and_then(|position| {
                let nth = layer.channels[..position].iter().filter(|c| decoded_channel(c.id)).count();
                decoded.get(nth).map(|(_, bytes)| bytes.as_slice())
            })
    };
    let (red, green, blue, alpha) = (
        find(CHANNEL_R),
        find(CHANNEL_G),
        find(CHANNEL_B),
        find(CHANNEL_TRANSPARENCY_MASK),
    );
    let user_mask = MaskSampler::new(layer, CHANNEL_LAYER_OR_VECTOR_MASK, find(CHANNEL_LAYER_OR_VECTOR_MASK));
    let layer_mask = MaskSampler::new(layer, CHANNEL_LAYER_MASK, find(CHANNEL_LAYER_MASK));

    let pixels = layer_width * layer_height;
    let mut rgba = vec![0u8; pixels * 4];
    for (i, out) in rgba.chunks_exact_mut(4).enumerate() {
        out[0] = red.and_then(|c| c.get(i)).copied().unwrap_or(0);
        out[1] = green.and_then(|c| c.get(i)).copied().unwrap_or(0);
        out[2] = blue.and_then(|c| c.get(i)).copied().unwrap_or(0);
        let mut a = alpha.and_then(|c| c.get(i)).copied().unwrap_or(255);
        for sampler in [&user_mask, &layer_mask].into_iter().flatten() {
            a = ((u32::from(a) * u32::from(sampler.sample(i)) + 127) / 255) as u8;
        }
        out[3] = a;
    }
    Ok(rgba)
}

/// A feathered mask channel plus the geometry needed to look up a layer pixel.
struct MaskSampler {
    mask: Mask,
    data: Vec<u8>,
    layer_left: i64,
    layer_top: i64,
    layer_width: usize,
}

impl MaskSampler {
    fn new(layer: &Layer, id: i16, data: Option<&[u8]>) -> Option<Self> {
        let mask = *mask_for_channel(layer, id)?;
        let data = data.filter(|data| !data.is_empty())?;
        let (width, height) = (mask.width(), mask.height());
        let data = if width > 0 && height > 0 {
            apply_mask_feather(data, width as usize, height as usize, mask.feather, mask.default_color)
        } else {
            data.to_vec()
        };
        Some(Self {
            mask,
            data,
            layer_left: i64::from(layer.left),
            layer_top: i64::from(layer.top),
            layer_width: layer.width() as usize,
        })
    }

    fn sample(&self, pixel_index: usize) -> u8 {
        let mask = &self.mask;
        let (width, height) = (mask.width(), mask.height());
        let mut value = mask.default_color;
        if width > 0 && height > 0 {
            let relative = mask.position_relative_to_layer;
            let x = (pixel_index % self.layer_width) as i64 + if relative { 0 } else { self.layer_left }
                - i64::from(mask.left);
            let y = (pixel_index / self.layer_width) as i64 + if relative { 0 } else { self.layer_top }
                - i64::from(mask.top);
            if x >= 0 && y >= 0 && x < width && y < height {
                if let Some(sample) = self.data.get((y * width + x) as usize) {
                    value = *sample;
                }
            }
        }
        apply_mask_settings(value, mask.density, mask.disabled, mask.invert)
    }
}

fn reserve_bytes(required: u64, max: u64, reserved: &mut u64) -> bool {
    if *reserved > max || required > max - *reserved {
        return false;
    }
    *reserved += required;
    true
}

fn reserve_rectangle(
    width: i64,
    height: i64,
    bytes_per_pixel: u64,
    max: u64,
    reserved: &mut u64,
    feather_line: bool,
) -> bool {
    if width < 0 || height < 0 || bytes_per_pixel == 0 {
        return false;
    }
    if width == 0 || height == 0 {
        return true;
    }
    let (width, height) = (width as u64, height as u64);
    let Some(bytes) = width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(bytes_per_pixel))
    else {
        return false;
    };
    if !reserve_bytes(bytes, max, reserved) {
        return false;
    }
    if !feather_line {
        return true;
    }
    // Six feather passes each allocate a prefix-sum line.
    let line_bytes = (width.max(height) + 1) * 8;
    reserve_bytes(line_bytes * 6, max, reserved)
}

/// Port of `ngReservePsdLayerExtraction`: reserves the RGBA output and mask working memory of one
/// layer against `max_bytes`. `retained` is only updated on success.
pub fn reserve_layer_extraction(layer: &Layer, layer_bytes_per_pixel: u64, max_bytes: u64, retained: &mut u64) -> bool {
    let mut proposed = *retained;
    let (layer_width, layer_height) = (i64::from(layer.width()), i64::from(layer.height()));
    if !reserve_rectangle(layer_width, layer_height, layer_bytes_per_pixel, max_bytes, &mut proposed, false) {
        return false;
    }
    for channel in &layer.channels {
        if channel.id != CHANNEL_LAYER_OR_VECTOR_MASK && channel.id != CHANNEL_LAYER_MASK {
            continue;
        }
        let (mut width, mut height) = (layer_width, layer_height);
        if let Some(mask) = mask_for_channel(layer, channel.id) {
            width = mask.width();
            height = mask.height();
        }
        if width < 0 || height < 0 {
            return false;
        }
        if width == 0 || height == 0 {
            width = layer_width;
            height = layer_height;
        }
        if !reserve_rectangle(width, height, MASK_EXTRACTION_BYTES_PER_PIXEL, max_bytes, &mut proposed, true) {
            return false;
        }
    }
    *retained = proposed;
    true
}
