//! Bounded VPD (Vocaloid Pose Data) reader.
//!
//! Shift-JIS text, written from the commonly documented format and never checked against a real file:
//!
//! ```text
//! Vocaloid Pose Data file
//! <model file name>;
//! <bone count>;
//! Bone0{<bone name>
//!   x,y,z;
//!   x,y,z,w;
//! }
//! ```
//!
//! `//` starts a comment that runs to the end of the line. Lines may end in LF or CRLF. Anything after the last
//! declared bone block (for example morph blocks) is ignored.

use super::sjis::MmdName;

pub const MAX_VPD_BYTES: usize = 1024 * 1024;
pub const MAX_VPD_BONES: usize = 4096;
/// Longest bone name or model file name, in bytes.
pub const MAX_VPD_NAME_BYTES: usize = 128;
const HEADER: &[u8] = b"Vocaloid Pose Data file";
const MIN_QUATERNION_LENGTH: f64 = 1e-6;

#[derive(Clone, Debug, PartialEq)]
pub struct VpdBone {
    pub name: MmdName,
    pub position: [f64; 3],
    /// x, y, z, w, unit length.
    pub rotation: [f64; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct Vpd {
    pub model_file: MmdName,
    pub bones: Vec<VpdBone>,
    /// Bones whose quaternion was exactly (0, 0, 0, 0); they read as the identity.
    pub zero_quaternions: u32,
}

fn trim(mut line: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = line {
        if first.is_ascii_whitespace() {
            line = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = line {
        if last.is_ascii_whitespace() {
            line = rest;
        } else {
            break;
        }
    }
    line
}

/// Strips a `//` comment and surrounding blanks. `/` (0x2F) is never a Shift-JIS trail byte, so a byte scan is safe.
fn clean(line: &[u8]) -> &[u8] {
    let end = line.windows(2).position(|pair| pair == b"//").unwrap_or(line.len());
    trim(&line[..end])
}

fn strip_semicolon<'a>(line: &'a [u8], what: &str) -> Result<&'a [u8], String> {
    match line.split_last() {
        Some((b';', rest)) => Ok(trim(rest)),
        _ => Err(format!("VPD {what} line must end with ';'")),
    }
}

fn numbers<const N: usize>(line: &[u8], bone: usize, what: &str) -> Result<[f64; N], String> {
    let text = std::str::from_utf8(strip_semicolon(line, what)?)
        .map_err(|_| format!("VPD bone {bone}: {what} is not numeric"))?;
    let mut out = [0.0; N];
    let mut parts = text.split(',');
    for slot in &mut out {
        let part = parts.next().ok_or_else(|| format!("VPD bone {bone}: {what} needs {N} numbers"))?;
        let value: f64 = part.trim().parse().map_err(|_| format!("VPD bone {bone}: {what} is not numeric"))?;
        if !value.is_finite() {
            return Err(format!("VPD bone {bone}: {what} is not finite"));
        }
        *slot = value;
    }
    if parts.next().is_some() {
        return Err(format!("VPD bone {bone}: {what} has more than {N} numbers"));
    }
    Ok(out)
}

pub fn parse_vpd(bytes: &[u8]) -> Result<Vpd, String> {
    if bytes.len() > MAX_VPD_BYTES {
        return Err(format!("VPD is larger than the {MAX_VPD_BYTES} byte limit"));
    }
    let mut lines = bytes.split(|&byte| byte == b'\n').map(clean);
    if lines.next() != Some(HEADER) {
        return Err("not a VPD file: unknown header".into());
    }
    // Only the lines the declared bones need are collected, so trailing text (morph blocks) costs nothing.
    let mut lines = lines.filter(|line| !line.is_empty());
    let (Some(model), Some(count)) = (lines.next(), lines.next()) else {
        return Err("VPD is truncated before the bone count".into());
    };
    let model_file = strip_semicolon(model, "model file")?;
    if model_file.len() > MAX_VPD_NAME_BYTES {
        return Err(format!("VPD model file name is longer than {MAX_VPD_NAME_BYTES} bytes"));
    }
    let model_file = MmdName::from_bytes(model_file);
    let count: usize = std::str::from_utf8(strip_semicolon(count, "bone count")?)
        .ok()
        .and_then(|text| text.parse().ok())
        .ok_or("VPD bone count is not a number")?;
    if count > MAX_VPD_BONES {
        return Err(format!("VPD declares {count} bones, above the limit {MAX_VPD_BONES}"));
    }
    let blocks: Vec<&[u8]> = lines.take(4 * count).collect();
    if blocks.len() < 4 * count {
        return Err(format!("VPD declares {count} bones but only {} lines remain", blocks.len()));
    }
    let mut zero_quaternions = 0;
    let mut bones = Vec::with_capacity(count);
    for (index, block) in blocks.chunks_exact(4).take(count).enumerate() {
        let name = block[0]
            .strip_prefix(b"Bone")
            .and_then(|rest| {
                let digits = rest.iter().take_while(|byte| byte.is_ascii_digit()).count();
                match rest[digits..].split_first() {
                    Some((b'{', name)) if digits > 0 => Some(trim(name)),
                    _ => None,
                }
            })
            .filter(|name| !name.is_empty())
            .ok_or_else(|| format!("VPD bone {index}: expected 'Bone<n>{{<name>'"))?;
        if name.len() > MAX_VPD_NAME_BYTES {
            return Err(format!("VPD bone {index}: name is longer than {MAX_VPD_NAME_BYTES} bytes"));
        }
        let position = numbers::<3>(block[1], index, "position")?;
        let mut rotation = numbers::<4>(block[2], index, "rotation")?;
        if block[3] != b"}" {
            return Err(format!("VPD bone {index}: expected '}}'"));
        }
        if rotation == [0.0; 4] {
            rotation = [0.0, 0.0, 0.0, 1.0];
            zero_quaternions += 1;
        }
        let length = rotation.iter().map(|value| value * value).sum::<f64>().sqrt();
        if length < MIN_QUATERNION_LENGTH {
            return Err(format!("VPD bone {index}: near-zero quaternion"));
        }
        for value in &mut rotation {
            *value /= length;
        }
        bones.push(VpdBone { name: MmdName::from_bytes(name), position, rotation });
    }
    Ok(Vpd { model_file, bones, zero_quaternions })
}
