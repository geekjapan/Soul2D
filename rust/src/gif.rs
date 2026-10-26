//! Animated GIF (GIF89a) writer for `export.gif`. The `image` build has no GIF codec, so this writes
//! the stream directly: a median-cut palette per frame, LZW, and the NETSCAPE loop extension.

use image::RgbaImage;
use std::collections::HashMap;

/// Pixels with alpha below this are transparent when the export keeps transparency.
const ALPHA_CUTOFF: u8 = 128;
const CHANNEL_BITS: u32 = 5;
const BINS: usize = 1 << (3 * CHANNEL_BITS);

/// ponytail: the whole file is held in memory; noisy frames near the export pixel cap can need over a gigabyte.
/// Stream to the file if that matters.
pub struct GifWriter {
    out: Vec<u8>,
    width: u16,
    height: u16,
    /// Hundredths of a second each frame is shown.
    delay: u16,
    /// Largest palette per frame, including the transparent slot when one is used.
    colors: usize,
    transparent: bool,
}

impl GifWriter {
    /// `repeat`: `None` loops forever, `Some(n)` plays the clip `n + 1` times (`Some(0)` once).
    pub fn new(
        width: u32,
        height: u32,
        delay: u16,
        colors: usize,
        transparent: bool,
        repeat: Option<u16>,
    ) -> Result<Self, String> {
        let (width, height) = match (u16::try_from(width), u16::try_from(height)) {
            (Ok(width), Ok(height)) if width > 0 && height > 0 => (width, height),
            _ => return Err("a GIF is at most 65535 pixels a side".into()),
        };
        let mut out = b"GIF89a".to_vec();
        out.extend(width.to_le_bytes());
        out.extend(height.to_le_bytes());
        // No global colour table: each frame carries its own.
        out.extend([0, 0, 0]);
        match repeat {
            Some(0) => {}
            other => {
                out.extend([0x21, 0xFF, 11]);
                out.extend(b"NETSCAPE2.0");
                out.extend([3, 1]);
                out.extend(other.unwrap_or(0).to_le_bytes());
                out.push(0);
            }
        }
        Ok(Self { out, width, height, delay, colors: colors.clamp(2, 256), transparent })
    }

    /// Appends `image`, which must be the size the writer was made with.
    pub fn add_frame(&mut self, image: &RgbaImage) -> Result<(), String> {
        if image.dimensions() != (u32::from(self.width), u32::from(self.height)) {
            return Err("a GIF frame changed size".into());
        }
        let opaque_slots = self.colors - usize::from(self.transparent);
        let (palette, indices) = quantize(image, opaque_slots, self.transparent);
        let table = palette.len().max(2).next_power_of_two();
        let bits = table.trailing_zeros() as u8;
        // Graphic control: disposal 2 clears a transparent frame before the next one.
        let disposal = if self.transparent { 2 << 2 } else { 1 << 2 };
        let flags = disposal | u8::from(self.transparent);
        self.out.extend([0x21, 0xF9, 4, flags]);
        self.out.extend(self.delay.to_le_bytes());
        // The transparent colour is index 0 when there is one.
        self.out.extend([0, 0]);
        self.out.push(0x2C);
        self.out.extend([0, 0, 0, 0]);
        self.out.extend(self.width.to_le_bytes());
        self.out.extend(self.height.to_le_bytes());
        self.out.push(0x80 | (bits - 1));
        for slot in 0..table {
            self.out.extend(palette.get(slot).copied().unwrap_or([0, 0, 0]));
        }
        self.out.push(bits.max(2));
        lzw(&indices, bits.max(2), &mut self.out);
        Ok(())
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.out.push(0x3B);
        self.out
    }
}

/// Median cut over a 5 bits per channel histogram. With `transparent`, index 0 is the transparent
/// slot and the opaque colours follow it.
fn quantize(image: &RgbaImage, slots: usize, transparent: bool) -> (Vec<[u8; 3]>, Vec<u8>) {
    let bin_of = |p: &image::Rgba<u8>| -> usize {
        let shift = 8 - CHANNEL_BITS;
        (usize::from(p[0] >> shift) << (2 * CHANNEL_BITS)) | (usize::from(p[1] >> shift) << CHANNEL_BITS)
            | usize::from(p[2] >> shift)
    };
    let mut histogram = vec![0u32; BINS];
    let mut sums = vec![[0u64; 3]; BINS];
    for pixel in image.pixels().filter(|p| !transparent || p[3] >= ALPHA_CUTOFF) {
        let bin = bin_of(pixel);
        histogram[bin] += 1;
        for (sum, channel) in sums[bin].iter_mut().zip(pixel.0) {
            *sum += u64::from(channel);
        }
    }
    let channel = |bin: usize, axis: usize| (bin >> ((2 - axis) as u32 * CHANNEL_BITS)) & ((1 << CHANNEL_BITS) - 1);
    let mut boxes: Vec<Vec<usize>> = vec![(0..BINS).filter(|bin| histogram[*bin] > 0).collect()];
    while boxes.len() < slots {
        // Split the box that spans the widest range of a channel.
        let widest = boxes
            .iter()
            .enumerate()
            .filter(|(_, members)| members.len() > 1)
            .map(|(index, members)| {
                (0..3)
                    .map(|axis| {
                        let values = members.iter().map(|bin| channel(*bin, axis));
                        let (low, high) = values.fold((usize::MAX, 0), |(l, h), v| (l.min(v), h.max(v)));
                        (high - low, axis, index)
                    })
                    .max()
                    .unwrap_or((0, 0, index))
            })
            .max();
        let Some((range, axis, index)) = widest else { break };
        if range == 0 {
            break;
        }
        let mut members = std::mem::take(&mut boxes[index]);
        members.sort_by_key(|bin| channel(*bin, axis));
        let total: u64 = members.iter().map(|bin| u64::from(histogram[*bin])).sum();
        let mut seen = 0u64;
        let mut cut = members.len() - 1;
        for (position, bin) in members.iter().enumerate() {
            seen += u64::from(histogram[*bin]);
            if seen * 2 >= total {
                cut = (position + 1).min(members.len() - 1);
                break;
            }
        }
        let upper = members.split_off(cut);
        boxes[index] = members;
        boxes.push(upper);
    }
    let offset = usize::from(transparent);
    let mut palette = vec![[0u8; 3]; offset];
    let mut index_of = vec![0u8; BINS];
    for (number, members) in boxes.iter().filter(|members| !members.is_empty()).enumerate() {
        let (mut sum, mut count) = ([0u64; 3], 0u64);
        for bin in members {
            for axis in 0..3 {
                sum[axis] += sums[*bin][axis];
            }
            count += u64::from(histogram[*bin]);
            index_of[*bin] = (number + offset) as u8;
        }
        palette.push([0, 1, 2].map(|axis| (sum[axis] / count) as u8));
    }
    let indices = image
        .pixels()
        .map(|pixel| if transparent && pixel[3] < ALPHA_CUTOFF { 0 } else { index_of[bin_of(pixel)] })
        .collect();
    (palette, indices)
}

/// GIF LZW with variable code width, emitted in sub-blocks of at most 255 bytes.
fn lzw(indices: &[u8], min_bits: u8, out: &mut Vec<u8>) {
    let clear = 1u32 << min_bits;
    let end = clear + 1;
    let mut packed = Vec::new();
    let (mut buffer, mut held) = (0u32, 0u32);
    let mut emit = |code: u32, width: u32, packed: &mut Vec<u8>| {
        buffer |= code << held;
        held += width;
        while held >= 8 {
            packed.push(buffer as u8);
            buffer >>= 8;
            held -= 8;
        }
    };
    let mut table: HashMap<(u32, u8), u32> = HashMap::new();
    let (mut next, mut width) = (end + 1, u32::from(min_bits) + 1);
    emit(clear, width, &mut packed);
    let mut prefix: Option<u32> = None;
    for &index in indices {
        let Some(current) = prefix else {
            prefix = Some(u32::from(index));
            continue;
        };
        if let Some(code) = table.get(&(current, index)) {
            prefix = Some(*code);
            continue;
        }
        emit(current, width, &mut packed);
        table.insert((current, index), next);
        // The decoder adds its entry one code later, so the width grows when `next` passes the limit.
        if next == (1 << width) && width < 12 {
            width += 1;
        }
        next += 1;
        if next == 4096 {
            emit(clear, width, &mut packed);
            table.clear();
            next = end + 1;
            width = u32::from(min_bits) + 1;
        }
        prefix = Some(u32::from(index));
    }
    if let Some(current) = prefix {
        emit(current, width, &mut packed);
        // The decoder grows its table after this code too.
        if next == (1 << width) && width < 12 {
            width += 1;
        }
    }
    emit(end, width, &mut packed);
    if held > 0 {
        packed.push(buffer as u8);
    }
    for block in packed.chunks(255) {
        out.push(block.len() as u8);
        out.extend(block);
    }
    out.push(0);
}
