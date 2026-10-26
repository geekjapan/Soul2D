//! Bounded VMD (Vocaloid Motion Data) reader and bone-track sampling.
//!
//! Layout (all little-endian), written from the commonly documented format and never checked against a real file:
//! a 30-byte magic field (`Vocaloid Motion Data 0002`, or the old `Vocaloid Motion Data file`), a model name (20
//! bytes, 10 in the old variant), then `u32 count` followed by that many bone keys of 111 bytes (15-byte name, `u32`
//! frame, 3 x `f32` position, 4 x `f32` quaternion x y z w, 64 bytes of interpolation), then `u32 count` followed by
//! morph keys of 23 bytes (15-byte name, `u32` frame, `f32` weight). Optional camera (61 bytes), light (28), shadow
//! (9) and IK (variable) sections may follow; they are skipped after checking that their counts fit the file.
//!
//! Interpolation block: 16 control bytes `X_x1 Y_x1 Z_x1 R_x1 | X_y1 .. | X_x2 .. | X_y2 ..` (element `4 * k + c` is
//! control value `k`, 0 x1, 1 y1, 2 x2, 3 y2, of channel `c`, 0 X, 1 Y, 2 Z, 3 rotation), written four times, row `r`
//! (at byte `16 * r`) shifted left by `r` elements and padded with 01 00 00. Each value is 0..=127, divided by 127.
//! Newer MMD overwrites bytes 2 and 3 (row 0, the Z and rotation x1 slots) with a physics flag (0x63, 0x0F), so
//! control `k` of channel `c` is read from the first element of its own row, byte `16 * c + 4 * k`, which equals
//! byte `4 * k + c` of a clean block and never touches bytes 2 and 3. The raw 64 bytes stay on every key. Never
//! checked against a real file.

use super::sjis::MmdName;
use std::collections::BTreeMap;

pub const MAX_VMD_BYTES: usize = 64 * 1024 * 1024;
/// About 9 hours at 30 fps.
pub const MAX_FRAME: u32 = 1_000_000;
pub const FRAMES_PER_SECOND: f64 = 30.0;

const MAGIC_FIELD: usize = 30;
const MAGIC_V2: &[u8] = b"Vocaloid Motion Data 0002";
const MAGIC_OLD: &[u8] = b"Vocaloid Motion Data file";
const NAME_FIELD: usize = 15;
const BONE_KEY_BYTES: usize = 111;
const MORPH_KEY_BYTES: usize = 23;
const CAMERA_KEY_BYTES: usize = 61;
const LIGHT_KEY_BYTES: usize = 28;
const SHADOW_KEY_BYTES: usize = 9;
/// frame u32, show u8, ik count u32; then `ik count` x (20-byte name, on/off u8).
const IK_HEADER_BYTES: usize = 9;
const IK_ENTRY_BYTES: usize = 21;
/// Non-zero quaternions shorter than this (after the f32 to f64 widening) are refused. Exactly zero reads as identity.
const MIN_QUATERNION_LENGTH: f64 = 1e-6;

/// Channel order of `BoneKey::curves`.
pub const CHANNEL_X: usize = 0;
pub const CHANNEL_Y: usize = 1;
pub const CHANNEL_Z: usize = 2;
pub const CHANNEL_ROTATION: usize = 3;

#[derive(Clone, Debug, PartialEq)]
pub struct BoneKey {
    pub name: MmdName,
    pub frame: u32,
    /// MMD units, left-handed Y-up.
    pub position: [f64; 3],
    /// x, y, z, w, unit length.
    pub rotation: [f64; 4],
    /// Per channel (X, Y, Z, rotation): Bezier control points `[x1, y1, x2, y2]` in 0..=1. The curve eases from the
    /// previous key to this one.
    pub curves: [[f64; 4]; 4],
    pub interpolation: [u8; 64],
}

#[derive(Clone, Debug, PartialEq)]
pub struct MorphKey {
    pub name: MmdName,
    pub frame: u32,
    pub weight: f64,
}

/// Entries of the sections that are skipped, so a caller can tell them apart from "none".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SkippedSections {
    pub camera: u32,
    pub light: u32,
    pub shadow: u32,
    pub ik: u32,
    /// Bytes after the last section that were not read.
    pub trailing_bytes: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vmd {
    pub model_name: MmdName,
    /// True for the `Vocaloid Motion Data file` header with a 10-byte model name.
    pub old_header: bool,
    /// In file order.
    pub bone_keys: Vec<BoneKey>,
    /// In file order.
    pub morph_keys: Vec<MorphKey>,
    pub skipped: SkippedSections,
    /// Bone keys whose quaternion was exactly (0, 0, 0, 0); they read as the identity.
    pub zero_quaternions: u32,
}

/// The keys of one bone, strictly increasing in frame (the last key of a duplicated frame wins).
#[derive(Clone, Debug, PartialEq)]
pub struct BoneTrack {
    pub name: MmdName,
    pub keys: Vec<BoneKey>,
}

impl Vmd {
    /// Groups bone keys by exact name bytes, sorted by frame. Tracks come out in byte order of the name.
    pub fn bone_tracks(&self) -> Vec<BoneTrack> {
        let mut groups: BTreeMap<&[u8], Vec<&BoneKey>> = BTreeMap::new();
        for key in &self.bone_keys {
            groups.entry(key.name.raw.as_slice()).or_default().push(key);
        }
        groups
            .into_values()
            .map(|mut keys| {
                // Stable sort keeps file order inside a frame, so the dedup below keeps the last one.
                keys.sort_by_key(|key| key.frame);
                let mut kept: Vec<BoneKey> = Vec::with_capacity(keys.len());
                for key in keys {
                    match kept.last_mut() {
                        Some(last) if last.frame == key.frame => *last = key.clone(),
                        _ => kept.push(key.clone()),
                    }
                }
                BoneTrack { name: kept[0].name.clone(), keys: kept }
            })
            .collect()
    }

    /// The (frame, weight) keys of the morph whose decoded name is `text`, strictly increasing in frame (the last key
    /// of a duplicated frame wins). Empty when the file has no such morph.
    pub fn morph_track(&self, text: &str) -> Vec<(u32, f64)> {
        let keys = self.morph_keys.iter().filter(|key| key.name.text == text).map(|key| (key.frame, key.weight));
        keys.collect::<BTreeMap<u32, f64>>().into_iter().collect()
    }

    /// First and last frame over bone and morph keys.
    pub fn frame_range(&self) -> Option<(u32, u32)> {
        let frames = self
            .bone_keys
            .iter()
            .map(|key| key.frame)
            .chain(self.morph_keys.iter().map(|key| key.frame));
        frames.fold(None, |range, frame| match range {
            None => Some((frame, frame)),
            Some((low, high)) => Some((low.min(frame), high.max(frame))),
        })
    }
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn remaining(&self) -> usize {
        self.bytes.len() - self.pos
    }

    fn take(&mut self, count: usize, what: &str) -> Result<&'a [u8], String> {
        if count > self.remaining() {
            return Err(format!("VMD is truncated in {what}"));
        }
        let slice = &self.bytes[self.pos..self.pos + count];
        self.pos += count;
        Ok(slice)
    }

    fn u32(&mut self, what: &str) -> Result<u32, String> {
        let bytes = self.take(4, what)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Reads a section count and checks `count * record` fits the rest of the file before anything is allocated.
    fn count(&mut self, record: usize, what: &str) -> Result<usize, String> {
        let count = self.u32(what)? as usize;
        match count.checked_mul(record) {
            Some(total) if total <= self.remaining() => Ok(count),
            _ => Err(format!(
                "VMD declares {count} {what} of {record} bytes but only {} bytes remain",
                self.remaining()
            )),
        }
    }
}

fn read_f32(bytes: &[u8], offset: usize) -> f64 {
    f64::from(f32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]))
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

fn label(name: &MmdName) -> String {
    name.label()
}

fn frame_checked(frame: u32, what: &str, index: usize, name: &MmdName) -> Result<u32, String> {
    if frame > MAX_FRAME {
        return Err(format!("{what} {index} ({}) has frame {frame} above the limit {MAX_FRAME}", label(name)));
    }
    Ok(frame)
}

pub fn parse_vmd(bytes: &[u8]) -> Result<Vmd, String> {
    if bytes.len() > MAX_VMD_BYTES {
        return Err(format!("VMD is larger than the {MAX_VMD_BYTES} byte limit"));
    }
    let mut cursor = Cursor { bytes, pos: 0 };
    let magic = cursor.take(MAGIC_FIELD, "the header")?;
    let magic = &magic[..magic.iter().position(|&byte| byte == 0).unwrap_or(MAGIC_FIELD)];
    let (old_header, name_bytes) = if magic == MAGIC_V2 {
        (false, 20)
    } else if magic == MAGIC_OLD {
        (true, 10)
    } else {
        return Err("not a VMD file: unknown header".into());
    };
    let model_name = MmdName::from_field(cursor.take(name_bytes, "the model name")?);

    let bone_count = cursor.count(BONE_KEY_BYTES, "bone keys")?;
    let mut bone_keys = Vec::with_capacity(bone_count);
    let mut zero_quaternions = 0;
    for index in 0..bone_count {
        let record = cursor.take(BONE_KEY_BYTES, "a bone key")?;
        let (key, was_zero) = parse_bone_key(record, index)?;
        zero_quaternions += u32::from(was_zero);
        bone_keys.push(key);
    }

    let mut morph_keys = Vec::new();
    let mut skipped = SkippedSections::default();
    // Very old files end after the bone keys, and older ones after the morph keys.
    if cursor.remaining() > 0 {
        let morph_count = cursor.count(MORPH_KEY_BYTES, "morph keys")?;
        morph_keys.reserve(morph_count);
        for index in 0..morph_count {
            let record = cursor.take(MORPH_KEY_BYTES, "a morph key")?;
            let name = MmdName::from_field(&record[..NAME_FIELD]);
            let frame = frame_checked(read_u32(record, NAME_FIELD), "morph key", index, &name)?;
            let weight = read_f32(record, NAME_FIELD + 4);
            if !weight.is_finite() {
                return Err(format!("morph key {index} ({}) has a non-finite weight", label(&name)));
            }
            morph_keys.push(MorphKey { name, frame, weight });
        }
    }
    if cursor.remaining() > 0 {
        skipped.camera = skip_fixed(&mut cursor, CAMERA_KEY_BYTES, "camera keys")?;
    }
    if cursor.remaining() > 0 {
        skipped.light = skip_fixed(&mut cursor, LIGHT_KEY_BYTES, "light keys")?;
    }
    if cursor.remaining() > 0 {
        skipped.shadow = skip_fixed(&mut cursor, SHADOW_KEY_BYTES, "shadow keys")?;
    }
    if cursor.remaining() > 0 {
        skipped.ik = skip_ik(&mut cursor)?;
    }
    skipped.trailing_bytes = cursor.remaining();

    Ok(Vmd { model_name, old_header, bone_keys, morph_keys, skipped, zero_quaternions })
}

fn skip_fixed(cursor: &mut Cursor, record: usize, what: &str) -> Result<u32, String> {
    let count = cursor.count(record, what)?;
    cursor.take(count * record, what)?;
    Ok(count as u32)
}

fn skip_ik(cursor: &mut Cursor) -> Result<u32, String> {
    let count = cursor.count(IK_HEADER_BYTES, "IK keys")?;
    for _ in 0..count {
        let header = cursor.take(IK_HEADER_BYTES, "an IK key")?;
        let entries = read_u32(header, 5) as usize;
        match entries.checked_mul(IK_ENTRY_BYTES) {
            Some(total) if total <= cursor.remaining() => {
                cursor.take(total, "an IK key")?;
            }
            _ => return Err("VMD IK key entries do not fit the remaining bytes".into()),
        }
    }
    Ok(count as u32)
}

/// The key and whether its quaternion was exactly zero (stored as the identity).
fn parse_bone_key(record: &[u8], index: usize) -> Result<(BoneKey, bool), String> {
    let name = MmdName::from_field(&record[..NAME_FIELD]);
    let frame = frame_checked(read_u32(record, NAME_FIELD), "bone key", index, &name)?;
    let mut position = [0.0; 3];
    for (axis, slot) in position.iter_mut().enumerate() {
        *slot = read_f32(record, 19 + 4 * axis);
    }
    let mut rotation = [0.0; 4];
    for (axis, slot) in rotation.iter_mut().enumerate() {
        *slot = read_f32(record, 31 + 4 * axis);
    }
    if position.iter().chain(rotation.iter()).any(|value| !value.is_finite()) {
        return Err(format!("bone key {index} ({}) has a non-finite value", label(&name)));
    }
    let was_zero = rotation == [0.0; 4];
    if was_zero {
        rotation = [0.0, 0.0, 0.0, 1.0];
    }
    let length = rotation.iter().map(|value| value * value).sum::<f64>().sqrt();
    if length < MIN_QUATERNION_LENGTH {
        return Err(format!("bone key {index} ({}) has a near-zero quaternion", label(&name)));
    }
    for value in &mut rotation {
        *value /= length;
    }
    let mut interpolation = [0u8; 64];
    interpolation.copy_from_slice(&record[47..111]);
    let mut curves = [[0.0; 4]; 4];
    for (channel, curve) in curves.iter_mut().enumerate() {
        for (slot, value) in curve.iter_mut().enumerate() {
            *value = f64::from(interpolation[16 * channel + 4 * slot].min(127)) / 127.0;
        }
    }
    Ok((BoneKey { name, frame, position, rotation, curves, interpolation }, was_zero))
}

/// Eases `s` in 0..=1 with the VMD Bezier whose end points are (0, 0) and (1, 1) and whose control points are
/// `[x1, y1, x2, y2]`. The x parameter is found by bisection (it is monotone for control x in 0..=1), 40 steps.
pub fn bezier_ease(curve: [f64; 4], s: f64) -> f64 {
    let [x1, y1, x2, y2] = curve;
    let s = s.clamp(0.0, 1.0);
    if (x1 == y1 && x2 == y2) || s == 0.0 || s == 1.0 {
        return s;
    }
    let cubic = |t: f64, p1: f64, p2: f64| {
        let u = 1.0 - t;
        3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t
    };
    let (mut low, mut high) = (0.0_f64, 1.0_f64);
    for _ in 0..40 {
        let mid = 0.5 * (low + high);
        if cubic(mid, x1, x2) < s {
            low = mid;
        } else {
            high = mid;
        }
    }
    cubic(0.5 * (low + high), y1, y2)
}

/// Spherical interpolation of unit quaternions (x, y, z, w), taking the short arc.
pub fn slerp(a: [f64; 4], b: [f64; 4], s: f64) -> [f64; 4] {
    let mut dot: f64 = a.iter().zip(&b).map(|(p, q)| p * q).sum();
    let mut b = b;
    if dot < 0.0 {
        dot = -dot;
        b = b.map(|value| -value);
    }
    let (weight_a, weight_b) = if dot > 0.9995 {
        (1.0 - s, s)
    } else {
        let angle = dot.clamp(-1.0, 1.0).acos();
        let sine = angle.sin();
        (((1.0 - s) * angle).sin() / sine, (s * angle).sin() / sine)
    };
    let mixed: [f64; 4] = std::array::from_fn(|axis| weight_a * a[axis] + weight_b * b[axis]);
    let length = mixed.iter().map(|value| value * value).sum::<f64>().sqrt();
    mixed.map(|value| value / length)
}

/// Position and rotation (x, y, z, w) of `track` at `frame` (30 fps frame numbers; fractional frames allowed).
/// Outside the keyed range the first or last key is held. An empty track (not produced by `bone_tracks`) samples as
/// the identity pose.
pub fn sample_bone(track: &BoneTrack, frame: f64) -> ([f64; 3], [f64; 4]) {
    let keys = &track.keys;
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        return ([0.0; 3], [0.0, 0.0, 0.0, 1.0]);
    };
    if !frame.is_finite() || frame <= f64::from(first.frame) {
        return (first.position, first.rotation);
    }
    if frame >= f64::from(last.frame) {
        return (last.position, last.rotation);
    }
    let next = keys.partition_point(|key| f64::from(key.frame) <= frame);
    let (from, to) = (&keys[next - 1], &keys[next]);
    let s = (frame - f64::from(from.frame)) / f64::from(to.frame - from.frame);
    let position = std::array::from_fn(|axis| {
        let eased = bezier_ease(to.curves[axis], s);
        from.position[axis] + (to.position[axis] - from.position[axis]) * eased
    });
    let rotation = slerp(from.rotation, to.rotation, bezier_ease(to.curves[CHANNEL_ROTATION], s));
    (position, rotation)
}

/// Weight of a `Vmd::morph_track` at `frame`: linear between two keys (a morph key carries no curve), the first or
/// last key outside the keyed range, 0 for an empty track.
pub fn sample_morph(keys: &[(u32, f64)], frame: f64) -> f64 {
    let (Some(first), Some(last)) = (keys.first(), keys.last()) else {
        return 0.0;
    };
    if !frame.is_finite() || frame <= f64::from(first.0) {
        return first.1;
    }
    if frame >= f64::from(last.0) {
        return last.1;
    }
    let next = keys.partition_point(|key| f64::from(key.0) <= frame);
    let (from, to) = (keys[next - 1], keys[next]);
    from.1 + (to.1 - from.1) * (frame - f64::from(from.0)) / f64::from(to.0 - from.0)
}

/// Frame number for a time in seconds at 30 fps.
pub fn frame_at_seconds(seconds: f64) -> f64 {
    seconds * FRAMES_PER_SECOND
}
