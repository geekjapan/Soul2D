//! Baseline and progressive JPEG decoder for image import (`import.folder`, `merge.images`, dropped files).
//!
//! The `image` build in this crate has no JPEG feature, so this module reads what D's `imagefmt` 2.1.2
//! accepted (sequential Huffman DCT, SOF0 and the identical 8-bit SOF1, one interleaved scan) and, beyond it,
//! progressive Huffman DCT (SOF2: spectral selection and successive approximation, buffered until EOI). Samples
//! are 8-bit, one or three components, sampling factors 1..4 with at most ten data units per MCU, restart
//! intervals. Arithmetic, lossless, hierarchical, 12-bit, CMYK and multi-scan sequential streams are refused with
//! an `Err` that names the feature. Chroma is upsampled by sample replication, so a decoder that interpolates
//! (libjpeg's "fancy" upsampling) differs on 4:2:0 and 4:2:2 edges.
//!
//! Hostile input: every table and byte access is bounds-checked, the frame size is validated against the
//! caller's limits before any sample buffer is allocated, and the entropy loop ends at the scan data.

use image::RgbaImage;

/// Real progressive files use about ten scans for a colour image (libjpeg's default script); 256 is generous.
const MAX_SCANS: usize = 256;
/// A scan costs one unit per block per coefficient of its band, whatever its entropy data holds (a block inside an
/// end-of-band run takes no bits but is still walked). The total may reach this many passes over every coefficient
/// of the frame: the usual script needs about three, a custom one with a refinement per bit plane needs eight.
const WORK_PASSES: u64 = 16;

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14,
    21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52, 45, 38, 31, 39, 46, 53,
    60, 61, 54, 47, 55, 62, 63,
];

#[derive(Clone, Default)]
struct Huffman {
    /// `max_code[len]`: largest code of that length, or -1 when none (index 0 unused).
    max_code: [i32; 17],
    /// `offset[len]`: index into `values` of the first code of that length, minus that code.
    offset: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn build(counts: &[u8], values: &[u8]) -> Result<Self, String> {
        let mut table = Self { values: values.to_vec(), ..Self::default() };
        let (mut code, mut index) = (0i32, 0i32);
        for len in 1..=16usize {
            let count = i32::from(counts[len - 1]);
            if count == 0 {
                table.max_code[len] = -1;
            } else {
                table.offset[len] = index - code;
                code += count;
                index += count;
                table.max_code[len] = code - 1;
                if code > (1 << len) {
                    return Err("JPEG Huffman table is over-subscribed".into());
                }
            }
            code <<= 1;
        }
        Ok(table)
    }
}

#[derive(Clone, Copy)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    quant: usize,
    dc: usize,
    ac: usize,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u32,
    count: u32,
    /// A real marker (not a stuffed 0xFF) was met at `pos`; no further data bytes exist.
    at_marker: bool,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos, buffer: 0, count: 0, at_marker: false }
    }

    fn bit(&mut self) -> Result<u32, String> {
        if self.count == 0 {
            if self.at_marker {
                return Err("JPEG scan data ended early".into());
            }
            let Some(&byte) = self.data.get(self.pos) else {
                return Err("JPEG scan data ended early".into());
            };
            if byte == 0xff {
                match self.data.get(self.pos + 1) {
                    Some(0) => {
                        self.pos += 2;
                        self.buffer = 0xff;
                    }
                    Some(_) => {
                        self.at_marker = true;
                        return Err("JPEG scan data ended early".into());
                    }
                    None => return Err("JPEG scan data ended early".into()),
                }
            } else {
                self.pos += 1;
                self.buffer = u32::from(byte);
            }
            self.count = 8;
        }
        self.count -= 1;
        Ok((self.buffer >> self.count) & 1)
    }

    fn receive(&mut self, size: u32) -> Result<i32, String> {
        let mut value = 0i32;
        for _ in 0..size {
            value = (value << 1) | self.bit()? as i32;
        }
        Ok(value)
    }

    /// T.81 F.2.2.1 EXTEND: a `size`-bit magnitude to a signed value.
    fn extend(&mut self, size: u32) -> Result<i32, String> {
        if size == 0 {
            return Ok(0);
        }
        let value = self.receive(size)?;
        Ok(if value < (1 << (size - 1)) { value - (1 << size) + 1 } else { value })
    }

    fn symbol(&mut self, table: &Huffman) -> Result<u8, String> {
        let mut code = 0i32;
        for len in 1..=16usize {
            code = (code << 1) | self.bit()? as i32;
            if table.max_code[len] >= 0 && code <= table.max_code[len] {
                let index = usize::try_from(code + table.offset[len]).map_err(|_| "corrupt JPEG Huffman code")?;
                return table.values.get(index).copied().ok_or_else(|| "corrupt JPEG Huffman code".to_owned());
            }
        }
        Err("corrupt JPEG Huffman code".into())
    }

    /// Drops the partial byte and consumes the `RSTn` marker that must follow the interval.
    fn restart(&mut self) -> Result<(), String> {
        self.count = 0;
        self.at_marker = false;
        // Fill bytes (0xFF 0xFF ...) may precede the marker.
        while self.data.get(self.pos) == Some(&0xff) && self.data.get(self.pos + 1) == Some(&0xff) {
            self.pos += 1;
        }
        match (self.data.get(self.pos), self.data.get(self.pos + 1)) {
            (Some(0xff), Some(0xd0..=0xd7)) => {
                self.pos += 2;
                Ok(())
            }
            _ => Err("JPEG restart marker is missing".into()),
        }
    }
}

fn u16_at(bytes: &[u8], at: usize) -> Result<usize, String> {
    match (bytes.get(at), bytes.get(at + 1)) {
        (Some(high), Some(low)) => Ok(usize::from(*high) << 8 | usize::from(*low)),
        _ => Err("JPEG segment is truncated".into()),
    }
}

struct Frame {
    width: usize,
    height: usize,
    components: Vec<Component>,
    /// Samples are already RGB (component ids `R`,`G`,`B` or Adobe transform 0): no YCbCr conversion.
    rgb: bool,
    /// SOF2: the image arrives as several scans of spectral bands that refine coefficients.
    progressive: bool,
}

fn parse_frame(payload: &[u8], max_side: u32, max_pixels: u64, progressive: bool) -> Result<Frame, String> {
    let length_error = || "JPEG frame header is malformed".to_owned();
    if *payload.first().ok_or_else(length_error)? != 8 {
        return Err("only 8-bit JPEG is supported".into());
    }
    let height = u16_at(payload, 1)?;
    let width = u16_at(payload, 3)?;
    let count = usize::from(*payload.get(5).ok_or_else(length_error)?);
    if width == 0 || height == 0 {
        return Err("JPEG has no size (DNL is not supported)".into());
    }
    if count != 1 && count != 3 {
        let kind = if count == 4 { " (CMYK)" } else { "" };
        return Err(format!("{count}-component{kind} JPEG is not supported"));
    }
    if payload.len() != 6 + count * 3 {
        return Err(length_error());
    }
    if width as u64 > u64::from(max_side)
        || height as u64 > u64::from(max_side)
        || width as u64 * height as u64 > max_pixels
    {
        return Err(format!("{width}x{height} is not a supported texture size"));
    }
    let mut components = Vec::new();
    let mut units = 0;
    for spec in payload[6..].chunks_exact(3) {
        let (h, v) = (usize::from(spec[1] >> 4), usize::from(spec[1] & 15));
        if !(1..=4).contains(&h) || !(1..=4).contains(&v) || spec[2] > 3 {
            return Err("unsupported JPEG sampling factors".into());
        }
        if components.iter().any(|c: &Component| c.id == spec[0]) {
            return Err("JPEG component ids repeat".into());
        }
        units += h * v;
        components.push(Component { id: spec[0], h, v, quant: usize::from(spec[2]), dc: 0, ac: 0 });
    }
    if units > 10 {
        return Err("unsupported JPEG sampling factors".into());
    }
    if count == 1 {
        // A single-component scan is never interleaved: one block per MCU whatever the factors say.
        components[0].h = 1;
        components[0].v = 1;
    }
    let rgb = components.iter().map(|c| c.id).eq(*b"RGB");
    Ok(Frame { width, height, components, rgb, progressive })
}

fn idct_table() -> [[f32; 8]; 8] {
    let mut table = [[0f32; 8]; 8];
    for (x, row) in table.iter_mut().enumerate() {
        for (u, cell) in row.iter_mut().enumerate() {
            let scale = if u == 0 { (1.0f32 / 8.0).sqrt() } else { 0.5 };
            *cell = scale * ((2 * x + 1) as f32 * u as f32 * std::f32::consts::PI / 16.0).cos();
        }
    }
    table
}

fn idct(coefficients: &[i32; 64], basis: &[[f32; 8]; 8]) -> [u8; 64] {
    let mut rows = [[0f32; 8]; 8];
    for v in 0..8 {
        for x in 0..8 {
            rows[v][x] = (0..8).map(|u| coefficients[v * 8 + u] as f32 * basis[x][u]).sum();
        }
    }
    let mut out = [0u8; 64];
    for y in 0..8 {
        for x in 0..8 {
            let sum: f32 = (0..8).map(|v| rows[v][x] * basis[y][v]).sum();
            out[y * 8 + x] = (sum + 128.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

struct Plane {
    stride: usize,
    samples: Vec<u8>,
}

/// Checks complete Huffman JPEG framing; pixel validation remains the decoder's job.
pub(crate) fn check_complete(bytes: &[u8]) -> Result<(), String> {
    let invalid = || "JPEG has incomplete or unsupported marker framing".to_owned();
    if bytes.get(..2) != Some(&[0xff, 0xd8]) { return Err(invalid()); }
    let (mut at, mut entropy, mut progressive, mut scans) = (2, false, None, 0);
    loop {
        if entropy {
            while bytes.get(at).is_some_and(|byte| *byte != 0xff) { at += 1; }
            let first = at;
            while bytes.get(at + 1) == Some(&0xff) { at += 1; }
            match bytes.get(at + 1) {
                Some(0x00) if at == first => { at += 2; continue; }
                Some(0xd0..=0xd7) => { at += 2; continue; }
                Some(0x00) => return Err(invalid()),
                Some(_) => entropy = false,
                None => return Err(invalid()),
            }
        }
        if bytes.get(at) != Some(&0xff) { return Err(invalid()); }
        while bytes.get(at + 1) == Some(&0xff) { at += 1; }
        let marker = *bytes.get(at + 1).ok_or_else(invalid)?;
        at += 2;
        match marker {
            0xd9 if scans > 0 && at == bytes.len() => return Ok(()),
            0x00 | 0x01 | 0xd0..=0xd9 | 0xdc => return Err(invalid()),
            _ => {}
        }
        let length = u16_at(bytes, at)?;
        if length < 2 || bytes.get(at..at + length).is_none() { return Err(invalid()); }
        if matches!(marker, 0xc0..=0xc2) && progressive.replace(marker == 0xc2).is_some() {
            return Err(invalid());
        }
        at += length;
        if marker == 0xda {
            let progressive = progressive.ok_or_else(invalid)?;
            scans += 1;
            if scans > MAX_SCANS || (!progressive && scans > 1) { return Err(invalid()); }
            entropy = true;
        }
    }
}

/// Decodes `bytes` to opaque RGBA. `max_side` and `max_pixels` bound the frame before any allocation.
pub fn decode(bytes: &[u8], max_side: u32, max_pixels: u64) -> Result<RgbaImage, String> {
    if bytes.get(..2) != Some(&[0xff, 0xd8]) {
        return Err("not a JPEG file".into());
    }
    let mut quant = [[1i32; 64]; 4];
    let mut dc_tables: [Option<Huffman>; 4] = Default::default();
    let mut ac_tables: [Option<Huffman>; 4] = Default::default();
    let mut restart_interval = 0usize;
    let mut frame: Option<Frame> = None;
    let mut coefficients: Option<Vec<Coefficients>> = None;
    // Work still allowed to progressive scans (set when the coefficient buffer is allocated), and the scans seen.
    let mut work_left = 0u64;
    let mut scans = 0usize;
    // T.81 B.2.4.1: a component's quantisation table is the one defined when its first scan starts.
    let mut bound: [Option<[i32; 64]>; 3] = [None; 3];
    // Components with a DC first scan: missing AC bands decode as zeros, but a missing DC band would fabricate a channel.
    let mut dc_seen = [false; 3];
    let mut adobe_rgb = false;
    let mut at = 2;
    loop {
        // Skip to the next marker; 0xFF fill bytes may repeat.
        while bytes.get(at) == Some(&0xff) && bytes.get(at + 1) == Some(&0xff) {
            at += 1;
        }
        if bytes.get(at) != Some(&0xff) {
            return Err("JPEG marker expected".into());
        }
        let marker = *bytes.get(at + 1).ok_or("JPEG ends inside a marker")?;
        if marker == 0xd9 {
            return match (frame, coefficients) {
                (Some(mut frame), Some(coefficients)) => {
                    if !dc_seen[..frame.components.len()].iter().all(|seen| *seen) {
                        return Err("progressive JPEG has no DC scan for a component".into());
                    }
                    frame.rgb |= adobe_rgb && frame.components.len() == 3;
                    finish_progressive(&frame, &coefficients, &quant, &bound)
                }
                _ => Err("JPEG has no image data".into()),
            };
        }
        if marker == 0x01 || (0xd0..=0xd8).contains(&marker) {
            at += 2;
            continue;
        }
        let length = u16_at(bytes, at + 2)?;
        if length < 2 {
            return Err("JPEG segment length is invalid".into());
        }
        let payload = bytes.get(at + 4..at + 2 + length).ok_or("JPEG segment is truncated")?;
        let next = at + 2 + length;
        match marker {
            // Adobe APP14: transform 0 on three components means the samples are RGB already.
            0xee if payload.starts_with(b"Adobe") && payload.len() >= 12 => adobe_rgb = payload[11] == 0,
            0xc0..=0xc2 => {
                if frame.is_some() {
                    return Err("JPEG has more than one frame".into());
                }
                frame = Some(parse_frame(payload, max_side, max_pixels, marker == 0xc2)?);
            }
            0xc9 | 0xca => return Err("arithmetic-coded JPEG is not supported".into()),
            0xc3 | 0xc5..=0xc7 | 0xcb | 0xcd..=0xcf => {
                return Err("lossless or hierarchical JPEG is not supported".into());
            }
            0xdb => {
                let mut rest = payload;
                while let Some((&info, tail)) = rest.split_first() {
                    let (wide, slot) = (info >> 4, usize::from(info & 15));
                    if wide > 1 || slot > 3 {
                        return Err("JPEG quantisation table is invalid".into());
                    }
                    let size = 64 * (1 + usize::from(wide));
                    let table = tail.get(..size).ok_or("JPEG quantisation table is truncated")?;
                    for (k, order) in ZIGZAG.iter().enumerate() {
                        quant[slot][*order] = if wide == 1 {
                            u16_at(table, k * 2)? as i32
                        } else {
                            i32::from(table[k])
                        };
                    }
                    rest = &tail[size..];
                }
            }
            0xc4 => {
                let mut rest = payload;
                while let Some((&info, tail)) = rest.split_first() {
                    let (class, slot) = (info >> 4, usize::from(info & 15));
                    if class > 1 || slot > 3 {
                        return Err("JPEG Huffman table is invalid".into());
                    }
                    let counts = tail.get(..16).ok_or("JPEG Huffman table is truncated")?;
                    let total: usize = counts.iter().map(|c| usize::from(*c)).sum();
                    if total > 256 {
                        return Err("JPEG Huffman table is invalid".into());
                    }
                    let values = tail.get(16..16 + total).ok_or("JPEG Huffman table is truncated")?;
                    let table = Some(Huffman::build(counts, values)?);
                    if class == 0 {
                        dc_tables[slot] = table;
                    } else {
                        ac_tables[slot] = table;
                    }
                    rest = &tail[16 + total..];
                }
            }
            0xdd => restart_interval = u16_at(payload, 0)?,
            0xda if frame.as_ref().is_some_and(|f| f.progressive) => {
                let frame = frame.as_ref().ok_or("JPEG scan precedes its frame header")?;
                let scan = progressive_scan_header(frame, payload)?;
                scans += 1;
                if scans > MAX_SCANS {
                    return Err("JPEG has too many scans".into());
                }
                let coefficients = coefficients.get_or_insert_with(|| {
                    let allocated = allocate_coefficients(frame);
                    work_left = allocated.iter().map(|c| c.blocks.len() as u64).sum::<u64>() * 64 * WORK_PASSES;
                    allocated
                });
                for &(index, _, _) in &scan.components {
                    dc_seen[index] |= scan.ss == 0 && scan.ah == 0;
                    bound[index].get_or_insert(quant[frame.components[index].quant]);
                }
                let tables = (&dc_tables, &ac_tables);
                let end = decode_progressive_scan(
                    frame,
                    coefficients,
                    bytes,
                    next,
                    &scan,
                    tables,
                    restart_interval,
                    &mut work_left,
                )?;
                at = next_marker(bytes, end);
                continue;
            }
            0xda => {
                let mut frame = frame.take().ok_or("JPEG scan precedes its frame header")?;
                let count = usize::from(*payload.first().ok_or("JPEG scan header is malformed")?);
                if count != frame.components.len() || payload.len() != 4 + count * 2 {
                    return Err("only single-scan JPEG is supported".into());
                }
                for (i, spec) in payload[1..1 + count * 2].chunks_exact(2).enumerate() {
                    // imagefmt requires the scan to list the components in frame order.
                    if frame.components[i].id != spec[0] || spec[1] >> 4 > 3 || spec[1] & 15 > 3 {
                        return Err("JPEG scan components do not match the frame".into());
                    }
                    frame.components[i].dc = usize::from(spec[1] >> 4);
                    frame.components[i].ac = usize::from(spec[1] & 15);
                }
                let spectral = &payload[1 + count * 2..];
                if spectral != [0, 63, 0] {
                    return Err("sequential JPEG scan selects a spectral band".into());
                }
                frame.rgb |= adobe_rgb && frame.components.len() == 3;
                return decode_scan(&frame, bytes, next, &quant, &dc_tables, &ac_tables, restart_interval);
            }
            _ => {}
        }
        at = next;
    }
}

fn decode_scan(
    frame: &Frame,
    bytes: &[u8],
    start: usize,
    quant: &[[i32; 64]; 4],
    dc_tables: &[Option<Huffman>; 4],
    ac_tables: &[Option<Huffman>; 4],
    restart_interval: usize,
) -> Result<RgbaImage, String> {
    let hmax = frame.components.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = frame.components.iter().map(|c| c.v).max().unwrap_or(1);
    let mcus_x = frame.width.div_ceil(8 * hmax);
    let mcus_y = frame.height.div_ceil(8 * vmax);
    let mut planes: Vec<Plane> = frame
        .components
        .iter()
        .map(|c| {
            let stride = mcus_x * c.h * 8;
            Plane { stride, samples: vec![0; stride * mcus_y * c.v * 8] }
        })
        .collect();
    let basis = idct_table();
    let mut bits = Bits::new(bytes, start);
    let mut previous_dc = vec![0i32; frame.components.len()];
    let mut since_restart = 0usize;
    for mcu_y in 0..mcus_y {
        for mcu_x in 0..mcus_x {
            if restart_interval > 0 && since_restart == restart_interval {
                bits.restart()?;
                previous_dc.iter_mut().for_each(|dc| *dc = 0);
                since_restart = 0;
            }
            since_restart += 1;
            for (index, component) in frame.components.iter().enumerate() {
                let dc_table = dc_tables[component.dc].as_ref().ok_or("JPEG DC Huffman table is missing")?;
                let ac_table = ac_tables[component.ac].as_ref().ok_or("JPEG AC Huffman table is missing")?;
                let q = &quant[component.quant];
                for block_y in 0..component.v {
                    for block_x in 0..component.h {
                        let mut coefficients = [0i32; 64];
                        let size = u32::from(bits.symbol(dc_table)?);
                        if size > 15 {
                            return Err("corrupt JPEG DC size".into());
                        }
                        previous_dc[index] = previous_dc[index].wrapping_add(bits.extend(size)?);
                        coefficients[0] = previous_dc[index].wrapping_mul(q[0]);
                        let mut k = 1;
                        while k < 64 {
                            let symbol = bits.symbol(ac_table)?;
                            let (run, size) = (usize::from(symbol >> 4), u32::from(symbol & 15));
                            if size == 0 {
                                if run == 15 {
                                    k += 16;
                                    continue;
                                }
                                break;
                            }
                            k += run;
                            if k > 63 {
                                return Err("corrupt JPEG coefficient run".into());
                            }
                            let natural = ZIGZAG[k];
                            coefficients[natural] = bits.extend(size)?.wrapping_mul(q[natural]);
                            k += 1;
                        }
                        let block = idct(&coefficients, &basis);
                        let plane = &mut planes[index];
                        let x0 = (mcu_x * component.h + block_x) * 8;
                        let y0 = (mcu_y * component.v + block_y) * 8;
                        for row in 0..8 {
                            let at = (y0 + row) * plane.stride + x0;
                            plane.samples[at..at + 8].copy_from_slice(&block[row * 8..row * 8 + 8]);
                        }
                    }
                }
            }
        }
    }
    to_rgba(frame, &planes, hmax, vmax)
}

fn to_rgba(frame: &Frame, planes: &[Plane], hmax: usize, vmax: usize) -> Result<RgbaImage, String> {
    let (width, height) = (frame.width, frame.height);
    let mut rgba = Vec::with_capacity(width * height * 4);
    for y in 0..height {
        for x in 0..width {
            let sample = |index: usize| {
                let component = &frame.components[index];
                let plane = &planes[index];
                plane.samples[(y * component.v / vmax) * plane.stride + x * component.h / hmax]
            };
            let pixel = if frame.components.len() == 1 {
                let gray = sample(0);
                [gray, gray, gray]
            } else if frame.rgb {
                [sample(0), sample(1), sample(2)]
            } else {
                let (luma, blue, red) = (f32::from(sample(0)), f32::from(sample(1)) - 128.0, f32::from(sample(2)) - 128.0);
                let clamp = |value: f32| value.round().clamp(0.0, 255.0) as u8;
                [
                    clamp(luma + 1.402 * red),
                    clamp(luma - 0.344_136 * blue - 0.714_136 * red),
                    clamp(luma + 1.772 * blue),
                ]
            };
            rgba.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 255]);
        }
    }
    RgbaImage::from_raw(width as u32, height as u32, rgba).ok_or_else(|| "JPEG size is inconsistent".to_owned())
}

// ---------------------------------------------------------------------------------------------
// Progressive (SOF2): scans refine a coefficient buffer, which is dequantised and transformed at EOI.
// ---------------------------------------------------------------------------------------------

/// Quantised coefficients of one component in natural (not zigzag) order, padded to whole MCUs.
struct Coefficients {
    stride: usize,
    blocks: Vec<[i16; 64]>,
}

struct ScanHeader {
    /// (frame component index, DC table, AC table) in scan order.
    components: Vec<(usize, usize, usize)>,
    ss: usize,
    se: usize,
    ah: u32,
    al: u32,
}

fn mcu_grid(frame: &Frame) -> (usize, usize, usize, usize) {
    let hmax = frame.components.iter().map(|c| c.h).max().unwrap_or(1);
    let vmax = frame.components.iter().map(|c| c.v).max().unwrap_or(1);
    (hmax, vmax, frame.width.div_ceil(8 * hmax), frame.height.div_ceil(8 * vmax))
}

fn allocate_coefficients(frame: &Frame) -> Vec<Coefficients> {
    let (_, _, mcus_x, mcus_y) = mcu_grid(frame);
    let blocks = |c: &Component| {
        let stride = mcus_x * c.h;
        Coefficients { stride, blocks: vec![[0; 64]; stride * mcus_y * c.v] }
    };
    frame.components.iter().map(blocks).collect()
}

fn progressive_scan_header(frame: &Frame, payload: &[u8]) -> Result<ScanHeader, String> {
    let malformed = || "JPEG scan header is malformed".to_owned();
    let count = usize::from(*payload.first().ok_or_else(malformed)?);
    if !(1..=frame.components.len()).contains(&count) || payload.len() != 4 + count * 2 {
        return Err(malformed());
    }
    let mut components = Vec::new();
    for spec in payload[1..1 + count * 2].chunks_exact(2) {
        let found = frame.components.iter().position(|c| c.id == spec[0]);
        let index = found.ok_or("JPEG scan component is not in the frame")?;
        if spec[1] >> 4 > 3 || spec[1] & 15 > 3 || components.iter().any(|(i, _, _)| *i == index) {
            return Err(malformed());
        }
        components.push((index, usize::from(spec[1] >> 4), usize::from(spec[1] & 15)));
    }
    let tail = &payload[1 + count * 2..];
    let (ss, se) = (usize::from(tail[0]), usize::from(tail[1]));
    let (ah, al) = (u32::from(tail[2] >> 4), u32::from(tail[2] & 15));
    // T.81 G.1.1.1.1: DC bands are never mixed with AC bands, AC bands are single-component, refinement is one bit.
    if ss == 0 && se != 0 {
        return Err("progressive JPEG scan mixes DC and AC coefficients".into());
    }
    if ss > 0 && (se < ss || se > 63) {
        return Err("progressive JPEG scan has an invalid spectral band".into());
    }
    if ss > 0 && count != 1 {
        return Err("progressive JPEG AC scan has more than one component".into());
    }
    if al > 13 || ah > 13 || (ah != 0 && ah != al + 1) {
        return Err("progressive JPEG scan has an invalid successive approximation".into());
    }
    Ok(ScanHeader { components, ss, se, ah, al })
}

/// Position of the next real marker (an `0xFF` followed by neither 0 nor `0xFF` nor `RSTn`) at or after `at`.
fn next_marker(bytes: &[u8], mut at: usize) -> usize {
    while at + 1 < bytes.len() {
        if bytes[at] == 0xff && !matches!(bytes[at + 1], 0 | 0xff | 0xd0..=0xd7) {
            return at;
        }
        at += 1;
    }
    bytes.len()
}

/// Decodes one progressive scan into `coefficients`; returns where its entropy data stopped.
fn decode_progressive_scan(
    frame: &Frame,
    coefficients: &mut [Coefficients],
    bytes: &[u8],
    start: usize,
    scan: &ScanHeader,
    (dc_tables, ac_tables): (&[Option<Huffman>; 4], &[Option<Huffman>; 4]),
    restart_interval: usize,
    work_left: &mut u64,
) -> Result<usize, String> {
    let (hmax, vmax, mcus_x, mcus_y) = mcu_grid(frame);
    let mut bits = Bits::new(bytes, start);
    let mut previous_dc = vec![0i32; frame.components.len()];
    let mut eob_run = 0u32;
    let interleaved = scan.components.len() > 1;
    // A lone component is scanned block by block over its own (unpadded) size, not by MCU.
    let (cols, rows) = match scan.components.first() {
        Some(&(index, _, _)) if !interleaved => {
            let c = &frame.components[index];
            ((frame.width * c.h).div_ceil(hmax).div_ceil(8), (frame.height * c.v).div_ceil(vmax).div_ceil(8))
        }
        _ => (mcus_x, mcus_y),
    };
    let blocks_per_unit: usize = scan
        .components
        .iter()
        .map(|&(i, _, _)| if interleaved { frame.components[i].h * frame.components[i].v } else { 1 })
        .sum();
    let cost = (cols * rows * blocks_per_unit) as u64 * (scan.se - scan.ss + 1) as u64;
    *work_left = work_left.checked_sub(cost).ok_or("JPEG has too many scans")?;
    for unit in 0..cols * rows {
        if restart_interval > 0 && unit > 0 && unit % restart_interval == 0 {
            bits.restart()?;
            previous_dc.iter_mut().for_each(|dc| *dc = 0);
            eob_run = 0;
        }
        let (x, y) = (unit % cols, unit / cols);
        for &(index, dc, ac) in &scan.components {
            let component = &frame.components[index];
            let (h, v) = if interleaved { (component.h, component.v) } else { (1, 1) };
            let dc_table = if scan.ss == 0 && scan.ah == 0 { dc_tables[dc].as_ref() } else { None };
            let ac_table = if scan.ss > 0 { ac_tables[ac].as_ref() } else { None };
            for block_y in 0..v {
                for block_x in 0..h {
                    let at = (y * v + block_y) * coefficients[index].stride + x * h + block_x;
                    let block = coefficients[index].blocks.get_mut(at).ok_or("JPEG block is outside the frame")?;
                    if scan.ss == 0 {
                        if scan.ah == 0 {
                            let table = dc_table.ok_or("JPEG DC Huffman table is missing")?;
                            let size = u32::from(bits.symbol(table)?);
                            if size > 15 {
                                return Err("corrupt JPEG DC size".into());
                            }
                            previous_dc[index] = previous_dc[index].wrapping_add(bits.extend(size)?);
                            block[0] = previous_dc[index].wrapping_shl(scan.al) as i16;
                        } else if bits.bit()? == 1 {
                            block[0] |= 1 << scan.al;
                        }
                    } else {
                        let table = ac_table.ok_or("JPEG AC Huffman table is missing")?;
                        if scan.ah == 0 {
                            ac_first(&mut bits, table, block, scan, &mut eob_run)?;
                        } else {
                            ac_refine(&mut bits, table, block, scan, &mut eob_run)?;
                        }
                    }
                }
            }
        }
    }
    Ok(bits.pos)
}

/// T.81 G.1.2.2: first pass over a spectral band.
fn ac_first(
    bits: &mut Bits,
    table: &Huffman,
    block: &mut [i16; 64],
    scan: &ScanHeader,
    eob_run: &mut u32,
) -> Result<(), String> {
    if *eob_run > 0 {
        *eob_run -= 1;
        return Ok(());
    }
    let mut k = scan.ss;
    while k <= scan.se {
        let symbol = bits.symbol(table)?;
        let (run, size) = (usize::from(symbol >> 4), u32::from(symbol & 15));
        if size != 0 {
            k += run;
            if k > scan.se {
                return Err("corrupt JPEG coefficient run".into());
            }
            block[ZIGZAG[k]] = bits.extend(size)?.wrapping_shl(scan.al) as i16;
            k += 1;
        } else if run == 15 {
            k += 16;
        } else {
            *eob_run = (1 << run) - 1;
            if run > 0 {
                *eob_run += bits.receive(run as u32)? as u32;
            }
            break;
        }
    }
    Ok(())
}

/// T.81 G.1.2.3: later pass; adds one bit of precision to coefficients already known and places new ones.
fn ac_refine(
    bits: &mut Bits,
    table: &Huffman,
    block: &mut [i16; 64],
    scan: &ScanHeader,
    eob_run: &mut u32,
) -> Result<(), String> {
    let (positive, negative) = (1i16 << scan.al, -1i16 << scan.al);
    let correct = |bits: &mut Bits, coefficient: &mut i16| -> Result<(), String> {
        if bits.bit()? == 1 && *coefficient & positive == 0 {
            *coefficient = coefficient.wrapping_add(if *coefficient >= 0 { positive } else { negative });
        }
        Ok(())
    };
    let mut k = scan.ss;
    if *eob_run == 0 {
        while k <= scan.se {
            let symbol = bits.symbol(table)?;
            let (mut run, size) = (i32::from(symbol >> 4), symbol & 15);
            let mut value = 0;
            if size != 0 {
                if size != 1 {
                    return Err("corrupt JPEG refinement size".into());
                }
                value = if bits.bit()? == 1 { positive } else { negative };
            } else if run != 15 {
                *eob_run = 1 << run;
                if run > 0 {
                    *eob_run += bits.receive(run as u32)? as u32;
                }
                break;
            }
            // Skip `run` zero coefficients, correcting every nonzero one passed on the way.
            while k <= scan.se {
                let coefficient = &mut block[ZIGZAG[k]];
                if *coefficient != 0 {
                    correct(bits, coefficient)?;
                } else {
                    run -= 1;
                    if run < 0 {
                        break;
                    }
                }
                k += 1;
            }
            if value != 0 {
                if k > scan.se {
                    return Err("corrupt JPEG coefficient run".into());
                }
                block[ZIGZAG[k]] = value;
            }
            k += 1;
        }
    }
    if *eob_run > 0 {
        while k <= scan.se {
            let coefficient = &mut block[ZIGZAG[k]];
            if *coefficient != 0 {
                correct(bits, coefficient)?;
            }
            k += 1;
        }
        *eob_run -= 1;
    }
    Ok(())
}

fn finish_progressive(
    frame: &Frame,
    coefficients: &[Coefficients],
    quant: &[[i32; 64]; 4],
    bound: &[Option<[i32; 64]>; 3],
) -> Result<RgbaImage, String> {
    let (hmax, vmax, _, mcus_y) = mcu_grid(frame);
    let basis = idct_table();
    let mut planes = Vec::new();
    for (index, (component, coded)) in frame.components.iter().zip(coefficients).enumerate() {
        let stride = coded.stride * 8;
        let mut samples = vec![0u8; stride * mcus_y * component.v * 8];
        let q = &bound[index].unwrap_or(quant[component.quant]);
        for (number, block) in coded.blocks.iter().enumerate() {
            let mut scaled = [0i32; 64];
            for (out, (value, factor)) in scaled.iter_mut().zip(block.iter().zip(q)) {
                *out = i32::from(*value).wrapping_mul(*factor);
            }
            let pixels = idct(&scaled, &basis);
            let (x0, y0) = (number % coded.stride * 8, number / coded.stride * 8);
            for row in 0..8 {
                let at = (y0 + row) * stride + x0;
                samples[at..at + 8].copy_from_slice(&pixels[row * 8..row * 8 + 8]);
            }
        }
        planes.push(Plane { stride, samples });
    }
    to_rgba(frame, &planes, hmax, vmax)
}

#[cfg(test)]
mod framing_tests {
    use super::check_complete;

    #[test]
    fn linked_resource_jpeg_accepts_filled_restart_markers() {
        check_complete(&[0xff, 0xd8, 0xff, 0xc0, 0, 2, 0xff, 0xda, 0, 2,
            17, 0xff, 0xff, 0xd0, 34, 0xff, 0xd9]).unwrap();
    }

    #[test]
    fn complete_jpeg_framing_skips_payloads_and_bounds_entropy() {
        // Deliberately minimal envelopes: frame and pixel semantics are tested by the decoder.
        let bytes = [0xff, 0xd8, 0xff, 0xe0, 0, 4, 0xff, 0xd9, 0xff, 0xc0, 0, 2,
            0xff, 0xda, 0, 2, 17, 0xff, 0, 34, 0xff, 0xd0, 51, 0xff, 0xff, 0xd9];
        check_complete(&bytes).unwrap();
        for length in 0..bytes.len() { assert!(check_complete(&bytes[..length]).is_err(), "{length}"); }
        let mut trailing = bytes.to_vec();
        trailing.push(0);
        assert!(check_complete(&trailing).is_err());
        for invalid in [
            vec![0xff, 0xd8, 0xff, 0xd8],
            vec![0xff, 0xd8, 0xff, 0xd0, 0xff, 0xd9],
            vec![0xff, 0xd8, 0xff, 0xe0, 0, 1, 0xff, 0xd9],
            vec![0xff, 0xd8, 0xff, 0xe0, 0, 8, 0xff, 0xd9],
        ] { assert!(check_complete(&invalid).is_err()); }
        for ending in [&[0xff, 0xff, 0, 0xff, 0xd9][..], &[0xff, 0xdc, 0, 4, 0, 1, 0xff, 0xd9][..]] {
            let mut invalid = bytes[..16].to_vec();
            invalid.extend_from_slice(ending);
            assert!(check_complete(&invalid).is_err());
        }
        let multiple = [0xff, 0xd8, 0xff, 0xc2, 0, 2, 0xff, 0xda, 0, 2, 17,
            0xff, 0xc4, 0, 3, 0, 0xff, 0xda, 0, 2, 34, 0xff, 0xd9];
        check_complete(&multiple).unwrap();
        let mut sequential = multiple;
        sequential[3] = 0xc0;
        assert!(check_complete(&sequential).is_err());
    }
}
