use crate::cv::WorkBudget;
use crate::render::MAX_TEXTURE_PIXELS;
use serde_json::{json, Value};

const QUERY_MAX_PIXELS: u64 = 65_536;
pub(crate) const MAX_ATLAS_PAGE: u32 = 8192;
const QUERY_BUDGET: u64 = 100_000_000;
const DEFAULT_ITERATIONS: i32 = 32;
const MAX_ITERATIONS: i32 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Mono,
    Rgb,
    Rgba,
}

impl PixelFormat {
    pub fn channels(&self) -> usize {
        match self {
            PixelFormat::Mono => 1,
            PixelFormat::Rgb => 3,
            PixelFormat::Rgba => 4,
        }
    }
}

#[derive(Clone, Debug)]
pub struct PixelImage {
    width: u32,
    height: u32,
    format: PixelFormat,
    pixels: Vec<u8>,
}

impl PixelImage {
    fn checked_pixel_count(width: u32, height: u32) -> Result<u64, String> {
        let total = (width as u64)
            .checked_mul(height as u64)
            .ok_or("image size overflows")?;
        if total > MAX_TEXTURE_PIXELS {
            return Err(format!(
                "image pixel count {total} exceeds limit {MAX_TEXTURE_PIXELS}"
            ));
        }
        Ok(total)
    }

    pub fn from_pixels(
        width: u32,
        height: u32,
        format: PixelFormat,
        pixels: Vec<u8>,
    ) -> Result<Self, String> {
        let total = Self::checked_pixel_count(width, height)?;
        let expected = total
            .checked_mul(format.channels() as u64)
            .ok_or("image size overflows")?;
        if pixels.len() as u64 != expected {
            return Err(format!(
                "image byte length {} does not match {width}x{height}x{}",
                pixels.len(),
                format.channels()
            ));
        }
        Ok(Self { width, height, format, pixels })
    }

/// A square atlas page of up to `MAX_ATLAS_PAGE` pixels a side: the one image allowed past the
    /// 16M-pixel texture cap (issue #151), since export writes it out and never decodes it back.
    pub(crate) fn atlas_page(size: u32) -> Result<Self, String> {
        if size == 0 || size > MAX_ATLAS_PAGE {
            return Err(format!("an atlas page must be 1 to {MAX_ATLAS_PAGE} pixels a side"));
        }
        let length = u64::from(size) * u64::from(size) * PixelFormat::Rgba.channels() as u64;
        Ok(Self { width: size, height: size, format: PixelFormat::Rgba, pixels: vec![0; length as usize] })
    }

    pub fn zeroed(width: u32, height: u32, format: PixelFormat) -> Result<Self, String> {
        let total = Self::checked_pixel_count(width, height)?;
        let length = (total * format.channels() as u64) as usize;
        Ok(Self {
            width,
            height,
            format,
            pixels: vec![0; length],
        })
    }

    pub fn size(&self) -> [u32; 2] {
        [self.width, self.height]
    }

    pub fn format(&self) -> PixelFormat {
        self.format
    }

    pub fn bit_depth(&self) -> u8 {
        8
    }

    pub fn channels(&self) -> usize {
        self.format.channels()
    }

    pub fn shape(&self) -> [u32; 3] {
        [self.height, self.width, self.channels() as u32]
    }

    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.pixels
    }

    pub fn bleed(
        &self,
        max_iterations: i32,
        budget: &mut WorkBudget,
    ) -> Result<PixelImage, String> {
        if self.format != PixelFormat::Rgba {
            return Err("color bleed requires an RGBA image".to_owned());
        }
        if max_iterations <= 0 || self.is_empty() {
            return Ok(self.clone());
        }
        let width = self.width as usize;
        let height = self.height as usize;
        let count = width * height;
        budget.charge(self.pixels.len() as u64)?;
        let mut image = self.pixels.clone();
        let mut known = vec![false; count];
        let mut pending: Vec<usize> = Vec::new();
        let mut completed: Vec<usize> = Vec::new();
        for index in 0..count {
            budget.charge(1)?;
            if image[index * 4 + 3] != 0 {
                known[index] = true;
            } else {
                pending.push(index);
            }
        }
        let mut iterations = 0i32;
        while !pending.is_empty() {
            if iterations >= max_iterations {
                break;
            }
            let last_pending = pending.len();
            let mut index = 0usize;
            while index < pending.len() {
                budget.charge(1)?;
                let pixel = pending[index];
                let x = (pixel % width) as i32;
                let y = (pixel / width) as i32;
                let mut r = 0u32;
                let mut g = 0u32;
                let mut b = 0u32;
                let mut sources = 0u32;
                for [dx, dy] in [
                    [-1, -1],
                    [0, -1],
                    [1, -1],
                    [-1, 0],
                    [1, 0],
                    [-1, 1],
                    [0, 1],
                    [1, 1],
                ] {
                    budget.charge(1)?;
                    let column = x + dx;
                    let row = y + dy;
                    if column < 0 || column >= width as i32 || row < 0 || row >= height as i32 {
                        continue;
                    }
                    let at = row as usize * width + column as usize;
                    if known[at] {
                        r += image[at * 4] as u32;
                        g += image[at * 4 + 1] as u32;
                        b += image[at * 4 + 2] as u32;
                        sources += 1;
                    }
                }
                if sources != 0 {
                    image[pixel * 4] = (r / sources) as u8;
                    image[pixel * 4 + 1] = (g / sources) as u8;
                    image[pixel * 4 + 2] = (b / sources) as u8;
                    image[pixel * 4 + 3] = 0;
                    completed.push(pixel);
                    pending[index] = pending[pending.len() - 1];
                    pending.pop();
                } else {
                    index += 1;
                }
            }
            for &pixel in &completed {
                budget.charge(1)?;
                known[pixel] = true;
            }
            completed.clear();
            iterations += 1;
            if pending.len() == last_pending {
                break;
            }
        }
        PixelImage::from_pixels(self.width, self.height, self.format, image)
    }
}

fn parse_format(request: &Value) -> Result<PixelFormat, String> {
    let Some(name) = request.get("format").and_then(Value::as_str) else {
        return Err("image.process requires a format string".to_owned());
    };
    match name {
        "Mono" => Ok(PixelFormat::Mono),
        "Rgb" => Ok(PixelFormat::Rgb),
        "Rgba" => Ok(PixelFormat::Rgba),
        other => Err(format!("unknown image format {other:?}")),
    }
}

fn parse_iterations(request: &Value) -> Result<i32, String> {
    match request.get("iterations") {
        None => Ok(DEFAULT_ITERATIONS),
        Some(value) => match value.as_i64() {
            Some(raw) if raw > MAX_ITERATIONS as i64 => {
                Err(format!("iterations must not exceed {MAX_ITERATIONS}"))
            }
            Some(raw) => i32::try_from(raw)
                .map_err(|_| "iterations is out of the i32 range".to_owned()),
            None => Err("iterations must be an integer".to_owned()),
        },
    }
}

pub(crate) fn process(request: &Value) -> Result<Value, String> {
    let Some(method) = request["method"].as_str() else {
        return Err("image.process requires a method string".to_owned());
    };
    if !matches!(method, "inspect" | "bleed") {
        return Err(format!("unknown image.process method {method:?}"));
    }
    let format = parse_format(request)?;
    let iterations = parse_iterations(request)?;
    let size = request["size"]
        .as_array()
        .filter(|size| size.len() == 2)
        .ok_or("image.process requires size [width, height]")?;
    let width = size[0]
        .as_u64()
        .filter(|value| *value <= QUERY_MAX_PIXELS)
        .ok_or("image width must be a nonnegative integer within the query limit")?;
    let height = size[1]
        .as_u64()
        .filter(|value| *value <= QUERY_MAX_PIXELS)
        .ok_or("image height must be a nonnegative integer within the query limit")?;
    let total = width * height;
    if total > QUERY_MAX_PIXELS {
        return Err(format!(
            "image.process pixel count {total} exceeds query limit {QUERY_MAX_PIXELS}"
        ));
    }
    let raw = request["pixels"]
        .as_array()
        .ok_or("image.process requires a pixels array")?;
    let expected = total * format.channels() as u64;
    if raw.len() as u64 != expected {
        return Err(format!(
            "pixels length {} does not match {width}x{height}x{}",
            raw.len(),
            format.channels()
        ));
    }
    let mut pixels = Vec::with_capacity(raw.len());
    for value in raw {
        let Some(pixel) = value.as_u64().filter(|pixel| *pixel <= u8::MAX as u64) else {
            return Err(format!("pixel {value} is not a u8 value"));
        };
        pixels.push(pixel as u8);
    }
    let image = PixelImage::from_pixels(width as u32, height as u32, format, pixels)?;
    let mut budget = WorkBudget::new(QUERY_BUDGET);
    let processed = match method {
        "inspect" => image,
        "bleed" => image.bleed(iterations, &mut budget)?,
        _ => unreachable!(),
    };
    let format_name = match processed.format() {
        PixelFormat::Mono => "Mono",
        PixelFormat::Rgb => "Rgb",
        PixelFormat::Rgba => "Rgba",
    };
    Ok(json!({
        "method": method,
        "size": processed.size(),
        "format": format_name,
        "bitDepth": processed.bit_depth(),
        "channels": processed.channels(),
        "shape": processed.shape(),
        "empty": processed.is_empty(),
        "pixels": processed.pixels(),
    }))
}
