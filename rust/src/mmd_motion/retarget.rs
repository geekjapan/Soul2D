//! Retargeting data and maths from MMD to the 19-bone standard depth skeleton. Pure functions, no `Session`.
//!
//! # Frames
//!
//! Rig (see `bone_deform::depth_edit_rotation`, whose matrix is `Rz(roll) Ry(-yaw) Rx(-pitch)`): x to the screen
//! right, y down (`skeleton.rs` puts the head at a smaller y than the pelvis), z toward the viewer (`depth_draw`
//! treats the larger depth as the front layer). That frame is left-handed as a physical frame, like MMD's.
//!
//! MMD: x to the screen right for a model that faces the camera, y up, z away from the camera (the model faces -z).
//!
//! The change of basis is `P = diag(1, -1, -1)`: the same physical directions written in each frame. It has
//! determinant +1, so a rotation `R` in MMD coordinates is `P R P^T` in rig coordinates, which for a quaternion
//! (x, y, z, w) is (x, -y, -z, w). The assumption that a VMD quaternion acts on MMD coordinates as the standard
//! rotation matrix of its (x, y, z, w) is not verified against a real file.
//!
//! # Left and right
//!
//! `skeleton.rs` places every `.L` bone at negative x (screen left) and every `.R` bone at positive x. MMD's 左 bones
//! sit at positive x (screen right for a camera-facing model). With x kept as x, MMD 左 therefore drives the rig's `.R`
//! bones, and MMD 右 drives `.L`: the retarget matches the picture, not the letter. The name table below is the only
//! place that decision lives; swap its `.L`/`.R` suffixes (and mirror x in `mmd_position_to_rig` and the quaternion
//! conversion) if the opposite reading is wanted.

use super::sjis::MmdName;
use super::vmd::Vmd;
use super::vpd::{Vpd, MAX_VPD_NAME_BYTES};
use crate::bone_deform::{depth_edit_rotation_wxyz, euler_from_depth_edit_wxyz};
use serde_json::Value;
use std::collections::BTreeMap;

/// Which part of an MMD bone pose drives the rig bone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    Rotation,
    Translation,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RigTarget {
    pub bone: &'static str,
    pub channel: Channel,
}

/// One standard MMD bone name; `target` is `None` where the 19-bone skeleton has no counterpart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoneMapping {
    pub mmd: &'static str,
    pub target: Option<RigTarget>,
    /// True where the MMD hierarchy differs from the rig's in a way playback must handle: 下半身 is a sibling of 上半身
    /// under センター in MMD, but the rig's Spine is a child of Pelvis, so a Pelvis rotation also swings the torso.
    /// `apply::plan` compensates for it; see "Hierarchy compensation" in `doc/mmd-motion.md`.
    pub sibling_of_upper_body_in_mmd: bool,
}

const fn rot(mmd: &'static str, bone: &'static str) -> BoneMapping {
    BoneMapping {
        mmd,
        target: Some(RigTarget { bone, channel: Channel::Rotation }),
        sibling_of_upper_body_in_mmd: false,
    }
}

const fn none(mmd: &'static str) -> BoneMapping {
    BoneMapping { mmd, target: None, sibling_of_upper_body_in_mmd: false }
}

/// Each rig bone channel has one source: Pelvis translation comes from センター and Pelvis rotation from 下半身.
/// グルーブ (an extra translation offset), 腰 (a model specific parent) and the IK, toe and eye bones are reported
/// as unmapped.
pub const BONE_TABLE: [BoneMapping; 30] = [
    none("全ての親"),
    BoneMapping {
        mmd: "センター",
        target: Some(RigTarget { bone: "Pelvis", channel: Channel::Translation }),
        sibling_of_upper_body_in_mmd: false,
    },
    none("グルーブ"),
    none("腰"),
    BoneMapping { sibling_of_upper_body_in_mmd: true, ..rot("下半身", "Pelvis") },
    rot("上半身", "Spine"),
    rot("上半身2", "Chest"),
    rot("首", "Neck"),
    rot("頭", "Head"),
    rot("左肩", "Clavicle.R"),
    rot("右肩", "Clavicle.L"),
    rot("左腕", "UpperArm.R"),
    rot("右腕", "UpperArm.L"),
    rot("左ひじ", "Forearm.R"),
    rot("右ひじ", "Forearm.L"),
    rot("左手首", "Hand.R"),
    rot("右手首", "Hand.L"),
    rot("左足", "Thigh.R"),
    rot("右足", "Thigh.L"),
    rot("左ひざ", "Shin.R"),
    rot("右ひざ", "Shin.L"),
    rot("左足首", "Foot.R"),
    rot("右足首", "Foot.L"),
    none("左足ＩＫ"),
    none("右足ＩＫ"),
    none("左つま先"),
    none("右つま先"),
    none("両目"),
    none("左目"),
    none("右目"),
];

/// Standard MMD morph names. None has a default parameter; `rig.bake_mmd_motion` maps morphs by its `morphs` argument.
pub const MORPH_VOCABULARY: [&str; 13] = [
    "まばたき",
    "ウィンク",
    "ウィンク右",
    "笑い",
    "あ",
    "い",
    "う",
    "え",
    "お",
    "怒り",
    "困る",
    "にこり",
    "真面目",
];

/// The table entry for `name`, if it is a standard MMD bone. A name with undecodable bytes never matches.
pub fn lookup_bone(name: &MmdName) -> Option<&'static BoneMapping> {
    BONE_TABLE.iter().find(|entry| entry.mmd == name.text)
}

/// At most this many entries in a `boneTable` override.
pub const MAX_BONE_TABLE_ENTRIES: usize = 64;
/// Longest key of a `boneTable` or `morphs` argument in UTF-8 bytes: the longest name a VPD file may hold
/// (`MAX_VPD_NAME_BYTES` Shift-JIS bytes) at 3 UTF-8 bytes per Shift-JIS byte, the most a character takes (a
/// half-width katakana byte is 3 bytes, a double-byte character 3 bytes for 2). A VMD name is 15 Shift-JIS bytes.
pub const MAX_MMD_NAME_BYTES: usize = 3 * MAX_VPD_NAME_BYTES;

/// A user-supplied name for an error message: at most 16 characters, control characters replaced by `?` (a stored
/// table is file content).
pub(crate) fn name_label(name: &str) -> String {
    name.chars().take(16).map(|c| if c.is_control() { '?' } else { c }).collect()
}

/// A key of `boneTable` or `morphs` is matched against decoded file names, so it must be text one byte name decodes
/// to: U+FFFD stands for any undecodable bytes and would match several.
pub(crate) fn check_mmd_name(argument: &str, name: &str) -> Result<(), String> {
    let usable = !name.is_empty()
        && name.len() <= MAX_MMD_NAME_BYTES
        && !name.chars().any(|c| c.is_control() || c == '\u{FFFD}');
    if usable {
        return Ok(());
    }
    Err(format!(
        "{argument} entry \"{}\" is not a usable name (1 to {MAX_MMD_NAME_BYTES} bytes, no control or U+FFFD characters)",
        name_label(name)
    ))
}

/// Overrides of `BONE_TABLE` by MMD bone name. A file bone is read under a *slot*: the standard MMD name of a
/// `BONE_TABLE` entry with a rig target. Without an override a standard name is its own slot; an override gives a
/// name outside the table the slot of a rig bone, or takes a name out (`None`). The hierarchy compensation of
/// `apply::plan` only ever sees slots, so it treats an overridden name as that rig bone's standard MMD counterpart,
/// parent chain included. See "Bone table override" in `doc/mmd-motion.md`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BoneTable {
    overrides: BTreeMap<String, Option<&'static str>>,
}

impl BoneTable {
    /// Parses `{"<MMD bone name>": "<rig boneId>" | null}`. Every refusal names the entry.
    pub fn parse(value: &Value) -> Result<Self, String> {
        let entries = value.as_object().filter(|entries| entries.len() <= MAX_BONE_TABLE_ENTRIES).ok_or_else(|| {
            format!("boneTable must be an object of at most {MAX_BONE_TABLE_ENTRIES} entries (MMD bone name to rig bone id or null)")
        })?;
        let mut overrides = BTreeMap::new();
        for (name, target) in entries {
            check_mmd_name("boneTable", name)?;
            let label = name_label(name);
            let slot = match target {
                Value::Null => None,
                Value::String(bone) => {
                    let slot = BONE_TABLE
                        .iter()
                        .find(|entry| entry.target.is_some_and(|t| t.bone == bone && t.channel == Channel::Rotation))
                        .map(|entry| entry.mmd)
                        .ok_or_else(|| {
                            format!("boneTable entry \"{label}\" names an unknown rig bone \"{}\"", name_label(bone))
                        })?;
                    // The compensation knows where a standard name sits in the MMD hierarchy (or that the rig has no
                    // counterpart for it); reading it under another bone's slot would contradict that.
                    if slot != name && BONE_TABLE.iter().any(|entry| entry.mmd == name) {
                        return Err(format!(
                            "boneTable entry \"{label}\" redirects a standard MMD bone, whose place in the hierarchy \
                             is fixed; a standard name can only be mapped to null"
                        ));
                    }
                    Some(slot)
                }
                _ => return Err(format!("boneTable entry \"{label}\" must be a rig bone id (a string) or null")),
            };
            overrides.insert(name.clone(), slot);
        }
        // One source per rig bone: a slot keeps its standard name unless that name was taken out.
        let mut sources: BTreeMap<&str, &str> = BONE_TABLE
            .iter()
            .filter(|entry| entry.target.is_some() && overrides.get(entry.mmd) != Some(&None))
            .map(|entry| (entry.mmd, entry.mmd))
            .collect();
        for (name, slot) in &overrides {
            let Some(slot) = slot.filter(|slot| slot != name) else {
                continue;
            };
            if let Some(other) = sources.insert(slot, name) {
                let bone = BONE_TABLE.iter().find(|entry| entry.mmd == slot).and_then(|entry| entry.target);
                return Err(format!(
                    "boneTable entry \"{}\" maps to {}, which \"{}\" already drives; map one of them to null",
                    name_label(name),
                    bone.map_or("", |target| target.bone),
                    name_label(other)
                ));
            }
        }
        Ok(Self { overrides })
    }

    /// The slot a file bone is read under, `None` for a bone that drives nothing.
    pub fn slot(&self, name: &MmdName) -> Option<&'static str> {
        match self.overrides.get(&name.text) {
            Some(slot) => *slot,
            None => lookup_bone(name).filter(|entry| entry.target.is_some()).map(|entry| entry.mmd),
        }
    }
}

/// MMD position to rig position (same units; scaling to rig size is left to the caller): (x, -y, -z).
pub fn mmd_position_to_rig(position: [f64; 3]) -> [f64; 3] {
    [position[0], -position[1], -position[2]]
}

/// MMD quaternion (x, y, z, w) to the rig quaternion (w, x, y, z): (w, x, -y, -z).
fn mmd_to_rig_wxyz(q: [f64; 4]) -> [f64; 4] {
    [q[3], q[0], -q[1], -q[2]]
}

/// Rig quaternion (w, x, y, z) to the MMD quaternion (x, y, z, w). The change of basis is its own inverse.
fn rig_wxyz_to_mmd(q: [f64; 4]) -> [f64; 4] {
    [q[1], -q[2], -q[3], q[0]]
}

/// Hamilton product of two `[w, x, y, z]` quaternions, the same convention as `bone_deform::mul_quat`.
pub(super) fn quat_mul_wxyz(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    [
        a[0] * b[0] - a[1] * b[1] - a[2] * b[2] - a[3] * b[3],
        a[0] * b[1] + a[1] * b[0] + a[2] * b[3] - a[3] * b[2],
        a[0] * b[2] - a[1] * b[3] + a[2] * b[0] + a[3] * b[1],
        a[0] * b[3] + a[1] * b[2] - a[2] * b[1] + a[3] * b[0],
    ]
}

/// Hamilton product of two MMD quaternions (x, y, z, w). The change of basis to the rig is a rotation conjugation, so
/// products agree in both frames.
pub fn mmd_quaternion_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let wxyz = |q: [f64; 4]| [q[3], q[0], q[1], q[2]];
    let product = quat_mul_wxyz(wxyz(a), wxyz(b));
    [product[1], product[2], product[3], product[0]]
}

/// Inverse of a unit MMD quaternion (x, y, z, w).
pub fn mmd_quaternion_inverse(q: [f64; 4]) -> [f64; 4] {
    [-q[0], -q[1], -q[2], q[3]]
}

fn unit_or_none<const N: usize>(q: [f64; N]) -> Option<[f64; N]> {
    let length = q.iter().map(|value| value * value).sum::<f64>().sqrt();
    (length.is_finite() && length != 0.0).then(|| q.map(|value| value / length))
}

/// The (pitch, yaw, roll) in radians, as `depth_edit_rotation` consumes them, whose rotation is the MMD quaternion
/// (x, y, z, w) written on the rig's world axes. This is NOT the pose a bone needs: a bone pose acts in the bone's
/// rest frame (see `mmd_quaternion_to_bone_pose`). Use this only for a bone whose rest orientation is the identity.
/// The input is normalised first. Inverse of `rig_world_pitch_yaw_roll_to_mmd_quaternion` for yaw inside
/// (-90, 90) degrees, pitch and roll inside (-180, 180]. At yaw of +-90 degrees the rotation has a one-parameter
/// family of (pitch, roll) answers; the rig's own rule is used (roll 0, the whole angle in pitch), so the result is
/// deterministic and rebuilds the same rotation. A zero-length or non-finite input gives `[0, 0, 0]`.
pub fn mmd_quaternion_to_rig_world_pitch_yaw_roll(q: [f64; 4]) -> [f64; 3] {
    let Some(unit) = unit_or_none(q) else {
        return [0.0; 3];
    };
    euler_from_depth_edit_wxyz(mmd_to_rig_wxyz(unit)).unwrap_or([0.0; 3])
}

/// The (pitch, yaw, roll) pose of a bone whose rest orientation is `rest` (`[w, x, y, z]`, as
/// `bone_deform::rest_quat_wxyz` returns it), for an MMD rotation (x, y, z, w) that is a rotation on the rig's world
/// axes. `bone_deform` poses a bone as `world = world_prefix * pose` and the prefix of a bone with unposed
/// ancestors is its rest orientation, so a world rotation `D` (`P q P`) needs `pose = rest^-1 * D * rest`. For an
/// upright bone the rest is a 180 degree turn about z, which flips pitch and yaw relative to the world-axes angles;
/// for a sideways bone it swaps them. Zero-length or non-finite `q` or `rest` gives `[0, 0, 0]`.
///
/// This treats an MMD bone rotation as a rotation on model axes (MMD bones have no rest roll), the same assumption as
/// the rest of the module. A child's pose stays relative to its parent because the rig's prefix carries the
/// parent's pose.
pub fn mmd_quaternion_to_bone_pose(rest: [f64; 4], q: [f64; 4]) -> [f64; 3] {
    let (Some(rest), Some(unit)) = (unit_or_none(rest), unit_or_none(q)) else {
        return [0.0; 3];
    };
    let rest_inverse = [rest[0], -rest[1], -rest[2], -rest[3]];
    let pose = quat_mul_wxyz(quat_mul_wxyz(rest_inverse, mmd_to_rig_wxyz(unit)), rest);
    euler_from_depth_edit_wxyz(pose).unwrap_or([0.0; 3])
}

/// MMD quaternion (x, y, z, w) of the rotation the rig builds from (pitch, yaw, roll) in radians, on the rig's world
/// axes (inverse of `mmd_quaternion_to_rig_world_pitch_yaw_roll`).
pub fn rig_world_pitch_yaw_roll_to_mmd_quaternion(pitch: f64, yaw: f64, roll: f64) -> [f64; 4] {
    rig_wxyz_to_mmd(depth_edit_rotation_wxyz(pitch, yaw, roll))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MappedBone {
    pub mmd: String,
    pub rig: &'static str,
    pub channel: Channel,
}

/// A name that is not retargeted. `hex` is the raw bytes; `standard` is true for a recognised MMD name (bone with no
/// rig counterpart, or any standard morph) as opposed to a name this decoder or table does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnmappedName {
    pub text: String,
    pub hex: String,
    pub standard: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetargetReport {
    pub mapped_bones: Vec<MappedBone>,
    pub unmapped_bones: Vec<UnmappedName>,
    pub unmapped_morphs: Vec<UnmappedName>,
    /// First and last frame of the file (`None` for a pose, or an empty motion).
    pub frame_range: Option<(u32, u32)>,
    /// Keys or bones whose quaternion was exactly zero and was read as the identity.
    pub zero_quaternions: u32,
}

fn dedup<'a>(names: impl Iterator<Item = &'a MmdName>) -> Vec<&'a MmdName> {
    let mut seen = std::collections::HashSet::new();
    names.filter(|name| seen.insert(name.raw.as_slice())).collect()
}

/// Builds the report for the bone and morph names of one file (each distinct raw name listed once, in file order).
pub fn report<'a>(
    table: &BoneTable,
    bones: impl Iterator<Item = &'a MmdName>,
    morphs: impl Iterator<Item = &'a MmdName>,
    frame_range: Option<(u32, u32)>,
    zero_quaternions: u32,
) -> RetargetReport {
    let mut mapped_bones = Vec::new();
    let mut unmapped_bones = Vec::new();
    for name in dedup(bones) {
        let slot = table.slot(name).and_then(|slot| BONE_TABLE.iter().find(|entry| entry.mmd == slot));
        match slot.and_then(|entry| entry.target) {
            Some(target) => mapped_bones.push(MappedBone {
                mmd: name.text.clone(),
                rig: target.bone,
                channel: target.channel,
            }),
            None => unmapped_bones.push(UnmappedName {
                text: name.text.clone(),
                hex: name.hex(),
                standard: lookup_bone(name).is_some(),
            }),
        }
    }
    let unmapped_morphs = dedup(morphs)
        .into_iter()
        .map(|name| UnmappedName {
            text: name.text.clone(),
            hex: name.hex(),
            standard: MORPH_VOCABULARY.contains(&name.text.as_str()),
        })
        .collect();
    RetargetReport { mapped_bones, unmapped_bones, unmapped_morphs, frame_range, zero_quaternions }
}

impl Vmd {
    pub fn retarget_report(&self, table: &BoneTable) -> RetargetReport {
        report(
            table,
            self.bone_keys.iter().map(|key| &key.name),
            self.morph_keys.iter().map(|key| &key.name),
            self.frame_range(),
            self.zero_quaternions,
        )
    }
}

impl Vpd {
    pub fn retarget_report(&self, table: &BoneTable) -> RetargetReport {
        report(table, self.bones.iter().map(|bone| &bone.name), std::iter::empty(), None, self.zero_quaternions)
    }
}
