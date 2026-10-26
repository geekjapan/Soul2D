use crate::cv::WorkBudget;
use crate::pixels::{PixelFormat, PixelImage, MAX_ATLAS_PAGE};
use serde_json::{json, Value};
use std::collections::BTreeMap;

const MAX_ATLAS_DIMENSION: u64 = 32768;
const MAX_RECTANGLES: usize = 4096;
// Atlas pages may pass the 16M-pixel texture cap up to 8192 a side (issue #151).
const MAX_BUILD_RESOLUTION: u32 = MAX_ATLAS_PAGE;
const MAX_BUILD_ATLASES: usize = 16;
const TEXTURE_SLOTS: usize = 3;

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub struct TexturePacker {
    width: u32,
    height: u32,
    used: Vec<Rect>,
    free: Vec<Rect>,
}

impl TexturePacker {
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err("atlas dimensions must be nonzero".into());
        }
        Ok(Self {
            width,
            height,
            used: Vec::new(),
            free: vec![Rect {
                x: 0,
                y: 0,
                width,
                height,
            }],
        })
    }

    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub fn pack(&mut self, width: u32, height: u32) -> Option<Rect> {
        if width == 0 || height == 0 {
            return None;
        }
        let node = self.find_best(width, height)?;
        self.place(node);
        Some(node)
    }

    /// Takes `rect` at its place, unless it overlaps a rectangle already taken.
    pub fn occupy(&mut self, rect: Rect) -> bool {
        let overlaps = |used: &Rect| {
            rect.x < used.x + used.width && used.x < rect.x + rect.width
                && rect.y < used.y + used.height && used.y < rect.y + rect.height
        };
        if rect.width == 0 || rect.height == 0 || self.used.iter().any(overlaps) {
            return false;
        }
        self.place(rect);
        true
    }

    pub fn remove(&mut self, rect: Rect) -> bool {
        let Some(index) = self.used.iter().position(|used| *used == rect) else {
            return false;
        };
        self.used.remove(index);
        self.free = vec![Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }];
        for index in 0..self.used.len() {
            let node = self.used[index];
            self.split_around(node);
        }
        true
    }

    pub fn clear(&mut self) {
        self.used.clear();
        self.free = vec![Rect {
            x: 0,
            y: 0,
            width: self.width,
            height: self.height,
        }];
    }

    pub fn occupancy(&self) -> f64 {
        let used: u64 = self
            .used
            .iter()
            .map(|rect| u64::from(rect.width) * u64::from(rect.height))
            .sum();
        used as f64 / (f64::from(self.width) * f64::from(self.height))
    }

    fn find_best(&self, width: u32, height: u32) -> Option<Rect> {
        let mut best = None;
        let mut best_short = u64::MAX;
        let mut best_long = u64::MAX;
        for free in &self.free {
            if free.width < width || free.height < height {
                continue;
            }
            let horizontal = u64::from(free.width - width);
            let vertical = u64::from(free.height - height);
            let short = horizontal.min(vertical);
            let long = horizontal.max(vertical);
            if short < best_short || (short == best_short && long < best_long) {
                best = Some(Rect {
                    x: free.x,
                    y: free.y,
                    width,
                    height,
                });
                best_short = short;
                best_long = long;
            }
        }
        best
    }

    fn place(&mut self, node: Rect) {
        self.split_around(node);
        self.used.push(node);
    }

    fn split_around(&mut self, node: Rect) {
        let mut index = 0;
        let mut pending = self.free.len();
        while index < pending {
            if self.split_free(index, node) {
                self.free.remove(index);
                pending -= 1;
            } else {
                index += 1;
            }
        }
        self.prune();
    }

    fn split_free(&mut self, index: usize, used: Rect) -> bool {
        let free = self.free[index];
        if used.x >= free.x + free.width
            || used.x + used.width <= free.x
            || used.y >= free.y + free.height
            || used.y + used.height <= free.y
        {
            return false;
        }
        if used.x < free.x + free.width && used.x + used.width > free.x {
            if used.y > free.y && used.y < free.y + free.height {
                self.free.push(Rect {
                    x: free.x,
                    y: free.y,
                    width: free.width,
                    height: used.y - free.y,
                });
            }
            if used.y + used.height < free.y + free.height {
                self.free.push(Rect {
                    x: free.x,
                    y: used.y + used.height,
                    width: free.width,
                    height: free.y + free.height - (used.y + used.height),
                });
            }
        }
        if used.y < free.y + free.height && used.y + used.height > free.y {
            if used.x > free.x && used.x < free.x + free.width {
                self.free.push(Rect {
                    x: free.x,
                    y: free.y,
                    width: used.x - free.x,
                    height: free.height,
                });
            }
            if used.x + used.width < free.x + free.width {
                self.free.push(Rect {
                    x: used.x + used.width,
                    y: free.y,
                    width: free.x + free.width
                        - (used.x + used.width),
                    height: free.height,
                });
            }
        }
        true
    }

    fn prune(&mut self) {
        let mut i = 0;
        while i < self.free.len() {
            let mut j = i + 1;
            let mut removed = false;
            while j < self.free.len() {
                if inside(self.free[i], self.free[j]) {
                    self.free.remove(i);
                    removed = true;
                    break;
                }
                if inside(self.free[j], self.free[i]) {
                    self.free.remove(j);
                } else {
                    j += 1;
                }
            }
            if !removed {
                i += 1;
            }
        }
    }
}

fn inside(inner: Rect, outer: Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.x + inner.width <= outer.x + outer.width
        && inner.y + inner.height <= outer.y + outer.height
}

pub(crate) fn plan(request: &Value) -> Result<Value, String> {
    let size = request
        .get("size")
        .and_then(Value::as_array)
        .filter(|size| size.len() == 2)
        .ok_or("size must contain [width, height]")?;
    let width = atlas_dimension(&size[0], "size[0]")?;
    let height = atlas_dimension(&size[1], "size[1]")?;
    let rectangles = request
        .get("rectangles")
        .and_then(Value::as_array)
        .ok_or("rectangles must be an array")?;
    if rectangles.is_empty() {
        return Err("rectangles must not be empty".into());
    }
    if rectangles.len() > MAX_RECTANGLES {
        return Err(format!("rectangles must not exceed {MAX_RECTANGLES} entries"));
    }
    let mut sizes = Vec::with_capacity(rectangles.len());
    for (index, entry) in rectangles.iter().enumerate() {
        let pair = entry
            .as_array()
            .filter(|pair| pair.len() == 2)
            .ok_or_else(|| format!("rectangles[{index}] must contain [width, height]"))?;
        sizes.push((
            extent(&pair[0], &format!("rectangles[{index}][0]"))?,
            extent(&pair[1], &format!("rectangles[{index}][1]"))?,
        ));
    }
    let mut packer = TexturePacker::new(width, height)?;
    let placements: Vec<Value> = sizes
        .iter()
        .map(|&(width, height)| match packer.pack(width, height) {
            Some(rect) => json!([rect.x, rect.y, rect.width, rect.height]),
            None => Value::Null,
        })
        .collect();
    Ok(json!({
        "size": [width, height],
        "placements": placements,
        "occupancy": packer.occupancy(),
    }))
}

fn atlas_dimension(value: &Value, path: &str) -> Result<u32, String> {
    let dimension = value
        .as_u64()
        .ok_or_else(|| format!("{path} must be an integer"))?;
    if dimension == 0 || dimension > MAX_ATLAS_DIMENSION {
        return Err(format!(
            "{path} must be between 1 and {MAX_ATLAS_DIMENSION}"
        ));
    }
    Ok(dimension as u32)
}

fn extent(value: &Value, path: &str) -> Result<u32, String> {
    value
        .as_u64()
        .and_then(|number| u32::try_from(number).ok())
        .filter(|number| *number != 0)
        .ok_or_else(|| format!("{path} must be a nonzero u32 integer"))
}

/// One part to atlas: slots are albedo, emissive, bump (D `TextureUsage`). Albedo is required.
pub struct AtlasPart<'a> {
    pub textures: [Option<&'a PixelImage>; TEXTURE_SLOTS],
    /// Mesh UVs, y down like the pixel rows.
    pub uvs: &'a [[f32; 2]],
    /// Part world bounds `[left, top, right, bottom]`; only drives the scale-to-fit decision.
    pub bounds: [f32; 4],
    /// A manual place in the layout (issue #151); `None` packs the part automatically.
    pub pin: Option<Pin>,
}

/// Where a part goes by hand: the top left of its pixels (padding excluded) on page `atlas`, at `scale` texels per
/// texture pixel instead of the packing scale.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Pin {
    pub atlas: usize,
    pub x: u32,
    pub y: u32,
    pub scale: Option<f32>,
}

/// The manual places of `model.atlasLayout.parts` (Part UUID to `{atlas, x, y, scale?}`, issue #151).
pub(crate) fn pins(model: &Value) -> Result<BTreeMap<u32, Pin>, String> {
    let Some(parts) = model.get("atlasLayout").and_then(|layout| layout.get("parts")) else {
        return Ok(BTreeMap::new());
    };
    let parts = parts.as_object().ok_or("atlasLayout.parts must be an object")?;
    parts.iter().map(|(id, place)| {
        let id = id.parse::<u32>().map_err(|_| format!("atlasLayout.parts key {id:?} is not a Part UUID"))?;
        Ok((id, parse_pin(place).map_err(|error| format!("atlasLayout.parts[{id}]: {error}"))?))
    }).collect()
}

fn parse_pin(place: &Value) -> Result<Pin, String> {
    let whole = |key: &str| place.get(key).and_then(Value::as_u64).and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| format!("{key} must be a non-negative integer"));
    let atlas = whole("atlas")? as usize;
    if atlas >= MAX_BUILD_ATLASES {
        return Err(format!("atlas must be below {MAX_BUILD_ATLASES}"));
    }
    let scale = match place.get("scale") {
        None | Some(Value::Null) => None,
        Some(value) => Some(value.as_f64().map(|scale| scale as f32).filter(|scale| *scale > 0.0 && *scale <= 16.0)
            .ok_or("scale must be greater than 0 and at most 16")?),
    };
    Ok(Pin { atlas, x: whole("x")?, y: whole("y")?, scale })
}

/// `atlas.layout.set {parts: [{id, atlas, x, y, scale?}]}` and `atlas.layout.clear {parts?}` on a copy of `model`.
pub(crate) fn edit_layout(model: &Value, request: &Value, op: &str) -> Result<Value, String> {
    let mut candidate = model.clone();
    if op == "atlas.layout.clear" && request.get("parts").is_none_or(Value::is_null) {
        candidate.as_object_mut().ok_or("model must be an object")?.remove("atlasLayout");
        return Ok(candidate);
    }
    let mut layout = pins(model)?.into_iter()
        .map(|(id, pin)| (id, json!({"atlas": pin.atlas, "x": pin.x, "y": pin.y, "scale": pin.scale})))
        .collect::<BTreeMap<_, _>>();
    let listed = request.get("parts").and_then(Value::as_array);
    if op == "atlas.layout.set" {
        let listed = listed.filter(|parts| !parts.is_empty() && parts.len() <= MAX_RECTANGLES)
            .ok_or_else(|| format!("parts must hold 1 to {MAX_RECTANGLES} places"))?;
        for place in listed {
            let id = place.get("id").and_then(Value::as_u64).and_then(crate::valid_id).ok_or("id must be a Part UUID")?;
            if crate::nodes::find_node(model, id).and_then(|node| node["type"].as_str()) != Some("Part") {
                return Err(format!("node {id} is not a Part"));
            }
            let pin = parse_pin(place)?;
            layout.insert(id, json!({"atlas": pin.atlas, "x": pin.x, "y": pin.y, "scale": pin.scale}));
        }
    } else if let Some(listed) = listed {
        for id in listed {
            let id = id.as_u64().and_then(|id| u32::try_from(id).ok()).ok_or("parts must hold Part UUIDs")?;
            layout.remove(&id).ok_or_else(|| format!("Part {id} has no manual place"))?;
        }
    } else {
        layout.clear();
    }
    let object = candidate.as_object_mut().ok_or("model must be an object")?;
    if layout.is_empty() {
        object.remove("atlasLayout");
    } else {
        let parts: serde_json::Map<String, Value> = layout.into_iter().map(|(id, place)| {
            let place = place.as_object().into_iter().flatten().filter(|(_, value)| !value.is_null())
                .map(|(key, value)| (key.clone(), value.clone())).collect();
            (id.to_string(), Value::Object(place))
        }).collect();
        object.insert("atlasLayout".into(), json!({"parts": parts}));
    }
    Ok(candidate)
}

/// The D export window options.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct AtlasSettings {
    pub resolution: u32,
    pub padding: i32,
    pub scale: f32,
    pub non_linear_scaling: bool,
}

impl Default for AtlasSettings {
    fn default() -> Self {
        Self { resolution: 2048, padding: 16, scale: 1.0, non_linear_scaling: false }
    }
}

pub struct AtlasImages {
    /// `None` when no part in this atlas uses the slot: D then nulls that slot on the parts.
    pub slots: [Option<PixelImage>; TEXTURE_SLOTS],
}

pub struct PartPlacement {
    pub atlas: usize,
    /// Rewritten UVs, normalised to the atlas.
    pub uvs: Vec<[f32; 2]>,
    /// Pixel rectangle `[x, y, width, height]` of the part inside the atlas, padding excluded.
    pub mapping: [u32; 4],
}

pub struct AtlasBuild {
    pub atlases: Vec<AtlasImages>,
    /// Same order as the input parts.
    pub parts: Vec<PartPlacement>,
    /// The uniform scale D forces on linear packing (`outputScale` of the export preview).
    pub output_scale: f32,
}

/// D `incINPExportGetPossiblePackingRatio`: the last part that overflows wins, as in D.
pub fn possible_packing_ratio(parts: &[AtlasPart], settings: &AtlasSettings) -> f32 {
    if settings.non_linear_scaling {
        return 1.0;
    }
    let mut rscale = 1.0f32;
    for part in parts {
        let (size, ratio) = packing_ratio(part, settings);
        if ratio[0] > 1.0 {
            rscale = overflow_scale(settings, size[0], 0.015);
        }
        if ratio[1] > 1.0 {
            rscale = overflow_scale(settings, size[1], 0.015);
        }
    }
    rscale
}

fn packing_ratio(part: &AtlasPart, settings: &AtlasSettings) -> ([f32; 2], [f32; 2]) {
    let padding = settings.padding as f32;
    let size = [
        (part.bounds[2] - part.bounds[0]) + padding,
        (part.bounds[3] - part.bounds[1]) + padding,
    ];
    let res = settings.resolution as f32;
    // The 0.01 literal is a D double, so the sum is rounded to float only once.
    let ratio = |extent: f32| ((extent * settings.scale / res) as f64 + 0.01) as f32;
    (size, [ratio(size[0]), ratio(size[1])])
}

fn overflow_scale(settings: &AtlasSettings, extent: f32, margin: f64) -> f32 {
    ((settings.resolution as f32 / extent) as f64 - margin) as f32
}

fn validate_build(parts: &[AtlasPart], settings: &AtlasSettings) -> Result<(), String> {
    if settings.resolution == 0 || settings.resolution > MAX_BUILD_RESOLUTION {
        return Err(format!("atlas resolution must be between 1 and {MAX_BUILD_RESOLUTION}"));
    }
    if settings.padding < 0 || settings.padding as u32 > settings.resolution {
        return Err("atlas padding must be between 0 and the atlas resolution".into());
    }
    if !settings.scale.is_finite() || settings.scale <= 0.0 || settings.scale > 16.0 {
        return Err("atlas scale must be greater than 0 and at most 16".into());
    }
    if parts.is_empty() || parts.len() > MAX_RECTANGLES {
        return Err(format!("atlas parts must contain 1 to {MAX_RECTANGLES} entries"));
    }
    for (index, part) in parts.iter().enumerate() {
        if part.textures[0].is_none() {
            return Err(format!("parts[{index}] has no albedo texture"));
        }
        for image in part.textures.iter().flatten() {
            let [width, height] = image.size();
            if image.format() != PixelFormat::Rgba || width == 0 || height == 0 {
                return Err(format!("parts[{index}] textures must be non-empty RGBA images"));
            }
        }
        if let Some(pin) = part.pin {
            if pin.atlas >= MAX_BUILD_ATLASES {
                return Err(format!("parts[{index}] is pinned past the {MAX_BUILD_ATLASES} atlases"));
            }
            if pin.scale.is_some_and(|scale| !scale.is_finite() || scale <= 0.0 || scale > 16.0) {
                return Err(format!("parts[{index}] pin scale must be greater than 0 and at most 16"));
            }
        }
        if part.bounds.iter().any(|value| !value.is_finite()) {
            return Err(format!("parts[{index}] bounds must be finite"));
        }
        if part.uvs.is_empty() || part.uvs.iter().flatten().any(|value| !value.is_finite()) {
            return Err(format!("parts[{index}] needs finite, non-empty uvs"));
        }
        let (min, max) = uv_bounds(part.uvs);
        if max[0] <= min[0] || max[1] <= min[1] {
            return Err(format!("parts[{index}] uvs cover no area"));
        }
    }
    Ok(())
}

// Starts from (1, 1, 0, 0) like D, so the box is not the true bounding box for UVs wholly outside 0..1.
fn uv_bounds(uvs: &[[f32; 2]]) -> ([f32; 2], [f32; 2]) {
    let mut min = [1.0f32, 1.0];
    let mut max = [0.0f32, 0.0];
    for uv in uvs {
        for axis in 0..2 {
            min[axis] = min[axis].min(uv[axis]);
            max[axis] = max[axis].max(uv[axis]);
        }
    }
    (min, max)
}

struct Canvas {
    slots: [Option<PixelImage>; TEXTURE_SLOTS],
    packer: TexturePacker,
    used: bool,
}

/// Port of `incINPExportGenAtlasses` + `Atlas.pack` + `incINPExportFinalizePacking`'s UV rewrite.
/// Fails when a part fits no empty atlas, or when the work budget runs out.
pub fn build_atlases(
    parts: &[AtlasPart],
    settings: &AtlasSettings,
    budget: &mut WorkBudget,
) -> Result<AtlasBuild, String> {
    validate_build(parts, settings)?;
    let optimal = possible_packing_ratio(parts, settings);
    let mut order: Vec<usize> = (0..parts.len()).collect();
    let weight = |index: usize| {
        let [width, height] = parts[index].textures[0].map_or([0, 0], PixelImage::size);
        u64::from(width) + u64::from(height)
    };
    order.sort_by_key(|&index| std::cmp::Reverse(weight(index)));

    let pages = parts.iter().filter_map(|part| part.pin).map(|pin| pin.atlas + 1).max().unwrap_or(1);
    let mut canvases = (0..pages).map(|_| new_canvas(settings.resolution)).collect::<Result<Vec<_>, _>>()?;
    let mut placements: Vec<Option<PartPlacement>> = (0..parts.len()).map(|_| None).collect();
    // Pinned parts take their places first; the others are packed around them.
    for (index, part) in parts.iter().enumerate() {
        if let Some(pin) = part.pin {
            placements[index] = Some(
                pack_part(part, settings, optimal, pin.atlas, &mut canvases[pin.atlas], budget)?
                    .ok_or_else(|| format!("parts[{index}] does not fit where it is pinned"))?,
            );
        }
    }
    let mut left = placements.iter().filter(|placement| placement.is_none()).count();
    let mut atlas = 0;
    while left > 0 {
        let mut packed = false;
        for &index in &order {
            if placements[index].is_some() {
                continue;
            }
            if let Some(placement) =
                pack_part(&parts[index], settings, optimal, atlas, &mut canvases[atlas], budget)?
            {
                placements[index] = Some(placement);
                left -= 1;
                packed = true;
                break;
            }
        }
        if !packed {
            if atlas + 1 < canvases.len() {
                atlas += 1;
                continue;
            }
            if !canvases[atlas].used {
                return Err("a texture is too big for the atlas".into());
            }
            if canvases.len() >= MAX_BUILD_ATLASES {
                return Err(format!("atlas count would exceed {MAX_BUILD_ATLASES}"));
            }
            budget.charge(u64::from(settings.resolution) * u64::from(settings.resolution))?;
            canvases.push(new_canvas(settings.resolution)?);
            atlas += 1;
        }
    }
    Ok(AtlasBuild {
        atlases: canvases.into_iter().map(|canvas| AtlasImages { slots: canvas.slots }).collect(),
        parts: placements.into_iter().flatten().collect(),
        output_scale: optimal,
    })
}

fn new_canvas(resolution: u32) -> Result<Canvas, String> {
    Ok(Canvas {
        slots: [None, None, None],
        packer: TexturePacker::new(resolution, resolution)?,
        used: false,
    })
}

// D `Atlas.pack` plus the UV rewrite; Ok(None) means "does not fit this atlas".
fn pack_part(
    part: &AtlasPart,
    settings: &AtlasSettings,
    optimal: f32,
    atlas: usize,
    canvas: &mut Canvas,
    budget: &mut WorkBudget,
) -> Result<Option<PartPlacement>, String> {
    let albedo = part.textures[0].ok_or("part has no albedo texture")?;
    let [tex_w, tex_h] = albedo.size();
    let (tex_w, tex_h) = (tex_w as f32, tex_h as f32);
    let mut rscale = optimal;
    if let Some(scale) = part.pin.and_then(|pin| pin.scale) {
        rscale = scale / settings.scale;
    } else if settings.non_linear_scaling {
        let (size, ratio) = packing_ratio(part, settings);
        if ratio[0] > 1.0 {
            rscale = overflow_scale(settings, size[0], 0.01);
        }
        if ratio[1] > 1.0 {
            rscale = overflow_scale(settings, size[1], 0.01);
        }
    }
    let fscale = rscale * settings.scale;

    let (mut area_min, mut area_max) = uv_bounds(part.uvs);
    let mut start = [0.0f32; 2];
    let mut end = [0.0f32; 2];
    for axis in 0..2 {
        if area_min[axis] < 0.0 {
            start[axis] = -area_min[axis];
            area_min[axis] = 0.0;
        }
        if area_max[axis] > 1.0 {
            end[axis] = area_max[axis] - 1.0;
            area_max[axis] = 1.0;
        }
    }
    let uv_rect = [
        area_min[0],
        area_min[1],
        area_max[0] - area_min[0],
        area_max[1] - area_min[1],
    ];
    let bounding = [uv_rect[2] + start[0] + end[0], uv_rect[3] + start[1] + end[1]];
    let padding = i64::from(settings.padding);
    // `as i64` truncates toward zero like D's `cast(int)` and saturates instead of overflowing.
    let width = (tex_w * bounding[0] * fscale) as i64 + padding * 2;
    let height = (tex_h * bounding[1] * fscale) as i64 + padding * 2;
    let resolution = i64::from(settings.resolution);
    if width <= 0 || height <= 0 {
        return Err("a part packs to an empty rectangle".into());
    }
    if width > resolution || height > resolution {
        return Ok(None);
    }
    let area = match part.pin {
        Some(pin) => {
            let area = Rect {
                x: pin.x.checked_sub(settings.padding as u32).ok_or("a pin must leave room for the padding")?,
                y: pin.y.checked_sub(settings.padding as u32).ok_or("a pin must leave room for the padding")?,
                width: width as u32,
                height: height as u32,
            };
            if i64::from(area.x) + width > resolution || i64::from(area.y) + height > resolution {
                return Ok(None);
            }
            if !canvas.packer.occupy(area) {
                return Err("a pinned part overlaps another pinned part".into());
            }
            area
        }
        None => match canvas.packer.pack(width as u32, height as u32) {
            Some(area) => area,
            None => return Ok(None),
        },
    };
    canvas.used = true;

    let pad = settings.padding as f32;
    start[0] *= tex_w * fscale;
    start[1] *= tex_h * fscale;
    end[0] *= tex_w * fscale;
    end[1] *= tex_h * fscale;
    let left = area.x as f32 + start[0] + pad;
    let top = area.y as f32 + start[1] + pad;
    let where_w = area.width as f32 - (padding * 2) as f32 - start[0] - end[0];
    let where_h = area.height as f32 - (padding * 2) as f32 - start[1] - end[1];
    let pad_u = settings.padding as u32;
    let mapping = [
        area.x + pad_u,
        area.y + pad_u,
        area.width - 2 * pad_u,
        area.height - 2 * pad_u,
    ];
    for (slot, texture) in part.textures.iter().enumerate() {
        let Some(texture) = texture else { continue };
        if canvas.slots[slot].is_none() {
            budget.charge(u64::from(settings.resolution) * u64::from(settings.resolution))?;
            canvas.slots[slot] = Some(PixelImage::atlas_page(settings.resolution)?);
        }
        let target = canvas.slots[slot].as_mut().ok_or("atlas slot missing")?;
        draw_quad(target, texture, [left, top, where_w, where_h], uv_rect, budget)?;
    }

    // Rewrite UVs: normalise inside the part's UV box, then place into the mapping rectangle.
    let (min, max) = uv_bounds(part.uvs);
    let atlas_size = settings.resolution as f32;
    let uvs = part
        .uvs
        .iter()
        .map(|uv| {
            let mut out = [0.0f32; 2];
            for axis in 0..2 {
                let off = ((uv[axis] - min[axis]) / (max[axis] - min[axis])).clamp(0.0, 1.0);
                out[axis] = (mapping[axis] as f32 + off * mapping[axis + 2] as f32) / atlas_size;
            }
            out
        })
        .collect();
    budget.charge(part.uvs.len() as u64)?;
    Ok(Some(PartPlacement { atlas, uvs, mapping }))
}

// The D GPU path: one textured quad, bilinear sampling, clamp to edge, destination pixels whose
// centres lie inside the quad. The destination is cleared to 0 and quads never overlap, so
// the "normal" blend is a plain copy. ponytail: no mipmaps, so heavy minification aliases
// slightly more than the GL sampler; add a box prefilter if that matters.
fn draw_quad(
    target: &mut PixelImage,
    source: &PixelImage,
    quad: [f32; 4],
    uv_rect: [f32; 4],
    budget: &mut WorkBudget,
) -> Result<(), String> {
    let [left, top, width, height] = quad;
    if width <= 0.0 || height <= 0.0 {
        return Ok(());
    }
    let [target_w, target_h] = target.size();
    let span = |from: f32, to: f32, limit: u32| {
        let first = (from - 0.5).ceil().max(0.0) as u32;
        let last = ((to - 0.5).ceil().max(0.0) as u32).min(limit);
        first..last.max(first)
    };
    let columns = span(left, left + width, target_w);
    let rows = span(top, top + height, target_h);
    budget.charge(columns.len() as u64 * rows.len() as u64)?;
    let [source_w, source_h] = source.size();
    for y in rows {
        let v = uv_rect[1] + (y as f32 + 0.5 - top) / height * uv_rect[3];
        for x in columns.clone() {
            let u = uv_rect[0] + (x as f32 + 0.5 - left) / width * uv_rect[2];
            let color = sample(source.pixels(), source_w, source_h, u, v);
            let at = (y as usize * target_w as usize + x as usize) * 4;
            for (channel, value) in color.iter().enumerate() {
                target.pixels_mut()[at + channel] = (value + 0.5).floor().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(())
}

fn sample(pixels: &[u8], width: u32, height: u32, u: f32, v: f32) -> [f32; 4] {
    let tx = u * width as f32 - 0.5;
    let ty = v * height as f32 - 0.5;
    let (x0, y0) = (tx.floor(), ty.floor());
    let (fx, fy) = (tx - x0, ty - y0);
    let index = |value: f32, limit: u32| (value.max(0.0) as u32).min(limit - 1) as usize;
    let (xa, xb) = (index(x0, width), index(x0 + 1.0, width));
    let (ya, yb) = (index(y0, height), index(y0 + 1.0, height));
    let texel = |x: usize, y: usize, channel: usize| f32::from(pixels[(y * width as usize + x) * 4 + channel]);
    let mut out = [0.0f32; 4];
    for (channel, slot) in out.iter_mut().enumerate() {
        let top = texel(xa, ya, channel) * (1.0 - fx) + texel(xb, ya, channel) * fx;
        let bottom = texel(xa, yb, channel) * (1.0 - fx) + texel(xb, yb, channel) * fx;
        *slot = top * (1.0 - fy) + bottom * fy;
    }
    out
}
