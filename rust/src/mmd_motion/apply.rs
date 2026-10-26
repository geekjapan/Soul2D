//! Plans an MMD pose (a VPD, or a VMD sampled at one frame) for the standard depth skeleton: the pitch, yaw and roll
//! of each mapped bone and the Pelvis translation. `Session::op_apply_mmd_pose` writes the plan into one parameter
//! keypoint; this module only reads the rig and computes. See "Applying a pose" in `doc/mmd-motion.md`.

use super::retarget::{
    mmd_position_to_rig, mmd_quaternion_inverse, mmd_quaternion_mul, mmd_quaternion_to_bone_pose, BoneTable, Channel,
    RetargetReport, BONE_TABLE,
};
use super::sjis::MmdName;
use super::vmd::{sample_bone, BoneTrack, Vmd};
use super::vpd::Vpd;
use crate::bone_deform::{bone_rest_info, BoneRestInfo};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Pelvis to Head distance of a typical MMD humanoid, in MMD units. ASSUMPTION, not measured on a real model: MMD
/// models are about 8 cm per unit, so a 1.58 m figure is about 20 units tall and its 下半身 to 頭 span is about 0.64 m.
/// The rig translation scale is the rig's own Pelvis to Head rest distance divided by this; `translationScale`
/// overrides it.
pub const NOMINAL_MMD_PELVIS_TO_HEAD: f64 = 8.0;

/// At most this many unmapped names are listed in a report; the counts are always exact.
const MAX_REPORTED_NAMES: usize = 256;

const CENTER: &str = "センター";
/// A rotation closer to the identity than this counts as no rotation (about 3e-6 rad).
const IDENTITY_TOLERANCE: f64 = 1e-12;
const IDENTITY: [f64; 4] = [0.0, 0.0, 0.0, 1.0];

/// One pose: for each slot of the bone table (a standard MMD bone that has a rig counterpart), the position and
/// rotation (x, y, z, w) of the file bone read under it.
pub struct Pose {
    pub format: &'static str,
    pub frame: Option<f64>,
    pub entries: BTreeMap<&'static str, ([f64; 3], [f64; 4])>,
    /// The file's own name of an entry that a bone table override reads under another name.
    pub renamed: BTreeMap<&'static str, String>,
    pub report: RetargetReport,
}

impl Pose {
    /// A bone listed twice keeps its last entry.
    pub fn from_vpd(vpd: &Vpd, table: &BoneTable) -> Self {
        let (mut entries, mut renamed) = (BTreeMap::new(), BTreeMap::new());
        for bone in &vpd.bones {
            if let Some(slot) = slot_of(table, &bone.name, &mut renamed) {
                entries.insert(slot, (bone.position, bone.rotation));
            }
        }
        Self { format: "vpd", frame: None, entries, renamed, report: vpd.retarget_report(table) }
    }

    pub fn from_vmd(vmd: &Vmd, frame: f64, table: &BoneTable) -> Self {
        Self::from_tracks(&vmd.bone_tracks(), vmd.retarget_report(table), frame, table)
    }

    /// `from_vmd` for many frames of one motion: the tracks (a sorted copy of every key) are built once.
    pub fn from_tracks(tracks: &[BoneTrack], report: RetargetReport, frame: f64, table: &BoneTable) -> Self {
        let (mut entries, mut renamed) = (BTreeMap::new(), BTreeMap::new());
        for track in tracks {
            if let Some(slot) = slot_of(table, &track.name, &mut renamed) {
                entries.entry(slot).or_insert_with(|| sample_bone(track, frame));
            }
        }
        Self { format: "vmd", frame: Some(frame), entries, renamed, report }
    }
}

fn slot_of(table: &BoneTable, name: &MmdName, renamed: &mut BTreeMap<&'static str, String>) -> Option<&'static str> {
    let slot = table.slot(name)?;
    if slot != name.text {
        renamed.insert(slot, name.text.clone());
    }
    Some(slot)
}

/// The rig bones the table can drive, by rig name, with their ids and rest data.
pub(crate) type RigBones = BTreeMap<&'static str, (u32, BoneRestInfo)>;

pub(crate) fn rig_bones(model: &Value, root_id: u32) -> Result<RigBones, String> {
    let by_name = crate::skeleton::bones_by_name(model, root_id)?;
    let mut bones = RigBones::new();
    for target in BONE_TABLE.iter().filter_map(|entry| entry.target) {
        if let Some(id) = by_name.get(target.bone) {
            bones.insert(target.bone, (*id, bone_rest_info(model, root_id, *id)?));
        }
    }
    Ok(bones)
}

pub struct RotationWrite {
    pub rig: &'static str,
    pub bone: u32,
    pub sources: Vec<&'static str>,
    /// Pitch, yaw, roll in radians.
    pub angles: [f64; 3],
    /// The file names this bone; otherwise the write replaces the stored value with the bone's MMD identity (plus
    /// whatever undoes its parent), so it does not depend on what the keypoint held before.
    pub named: bool,
    /// The write is the identity pose (all angles zero).
    pub identity: bool,
}

pub struct TranslationWrite {
    pub bone: u32,
    /// Rig units; zero when the file does not name センター.
    pub value: [f64; 3],
    /// The file names センター.
    pub named: bool,
}

pub struct Plan {
    pub rotations: Vec<RotationWrite>,
    pub translation: Option<TranslationWrite>,
    /// MMD units to rig units; `None` when the rig has no usable Pelvis to Head span and no override was given.
    pub scale: Option<f64>,
    /// Rig bones the pose drives that this rig lacks.
    pub missing_in_rig: Vec<&'static str>,
    /// Rig bones whose rotation or translation lock made the plan skip them.
    pub locked: Vec<&'static str>,
}

fn mmd_parent(name: &str) -> Option<&'static str> {
    Some(match name {
        "下半身" | "上半身" => CENTER,
        "上半身2" => "上半身",
        "首" | "左肩" | "右肩" => "上半身2",
        "頭" => "首",
        "左腕" => "左肩",
        "右腕" => "右肩",
        "左ひじ" => "左腕",
        "右ひじ" => "右腕",
        "左手首" => "左ひじ",
        "右手首" => "右ひじ",
        "左足" | "右足" => "下半身",
        "左ひざ" => "左足",
        "右ひざ" => "右足",
        "左足首" => "左ひざ",
        "右足首" => "右ひざ",
        _ => return None,
    })
}

/// The bone, then its ancestors in the standard MMD hierarchy, ending at センター.
fn mmd_chain(name: &'static str) -> Vec<&'static str> {
    let mut chain = vec![name];
    while let Some(next) = mmd_parent(chain[chain.len() - 1]) {
        chain.push(next);
    }
    chain
}

fn default_scale(bones: &RigBones) -> Option<f64> {
    let (pelvis, head) = (bones.get("Pelvis")?, bones.get("Head")?);
    let span = (0..3).map(|axis| (pelvis.1.head[axis] - head.1.head[axis]).powi(2)).sum::<f64>().sqrt();
    (span > 1e-6).then(|| span / NOMINAL_MMD_PELVIS_TO_HEAD)
}

pub(crate) fn plan(pose: &Pose, bones: &RigBones, scale_override: Option<f64>) -> Plan {
    let rotation_of = |name: &str| pose.entries.get(name).map_or(IDENTITY, |(_, rotation)| *rotation);
    // MMD world rotation of a bone: the product of the parent-relative rotations from センター down to it.
    let mmd_world = |name: &'static str| {
        mmd_chain(name).iter().rev().fold(IDENTITY, |world, bone| mmd_quaternion_mul(world, rotation_of(bone)))
    };
    // By rig bone id: its MMD name and the world rotation it ends up with, so a child can undo what its parent does.
    let mut actual: BTreeMap<u32, (&'static str, [f64; 4])> = BTreeMap::new();

    let mut plan = Plan {
        rotations: Vec::new(),
        translation: None,
        scale: scale_override.or_else(|| default_scale(bones)),
        missing_in_rig: Vec::new(),
        locked: Vec::new(),
    };
    for entry in BONE_TABLE {
        let Some(target) = entry.target.filter(|target| target.channel == Channel::Rotation) else {
            continue;
        };
        let listed = pose.entries.contains_key(entry.mmd);
        let Some((id, info)) = bones.get(target.bone) else {
            if listed {
                plan.missing_in_rig.push(target.bone);
            }
            continue;
        };
        // Hierarchy compensation. The rig poses a bone as world = parent_world * rest_parent^-1 * rest * pose, or
        // rest * pose when it ignores its parent. With D = world * rest^-1 (a rotation on world axes) that reads
        // D(bone) = D(parent) * L, L = rest * pose * rest^-1 (no D(parent) for a bone that ignores its parent).
        // MMD wants D(bone) = mmd_world(bone), so L = D(rig parent)^-1 * mmd_world(bone), pose = rest^-1 * L * rest.
        // Usually D(rig parent) = mmd_world(MMD parent) and L is the file's own rotation. Spine differs: its rig
        // parent Pelvis carries センター 下半身 but 上半身 hangs under センター, so L = 下半身^-1 上半身.
        let parent = info.parent.filter(|_| info.follows_parent).and_then(|parent| actual.get(&parent).copied());
        let parent_world = parent.map_or(IDENTITY, |(_, world)| world);
        let wanted = mmd_quaternion_mul(mmd_quaternion_inverse(parent_world), mmd_world(entry.mmd));
        let local = if info.lock_rotation { IDENTITY } else { wanted };
        actual.insert(*id, (entry.mmd, mmd_quaternion_mul(parent_world, local)));
        if info.lock_rotation {
            // Also when the file does not name it but the lock suppressed a compensation (Spine after a 下半身 turn).
            if listed || 1.0 - wanted[3].abs() > IDENTITY_TOLERANCE {
                plan.locked.push(target.bone);
            }
            continue;
        }
        // Every bone the table can drive is written, named or not, so applying a pose replaces the keypoint's pose.
        let identity = 1.0 - local[3].abs() <= IDENTITY_TOLERANCE;
        let mut sources: Vec<&'static str> = Vec::new();
        let chains = mmd_chain(entry.mmd).into_iter().chain(parent.into_iter().flat_map(|(name, _)| mmd_chain(name)));
        for name in chains {
            if pose.entries.contains_key(name) && !sources.contains(&name) {
                sources.push(name);
            }
        }
        let angles = if identity && !listed { [0.0; 3] } else { mmd_quaternion_to_bone_pose(info.rest, local) };
        plan.rotations.push(RotationWrite { rig: target.bone, bone: *id, sources, angles, named: listed, identity });
    }
    let centre = pose.entries.get(CENTER);
    match bones.get("Pelvis") {
        None => {
            if centre.is_some() {
                plan.missing_in_rig.push("Pelvis");
            }
        }
        Some((_, info)) if info.lock_translation => {
            if centre.is_some() {
                plan.locked.push("Pelvis");
            }
        }
        // Translation is replaced like rotation: zero when the file does not name センター.
        Some((id, _)) => match (centre, plan.scale) {
            (Some((position, _)), Some(scale)) => {
                let value = mmd_position_to_rig(*position).map(|value| value * scale);
                plan.translation = Some(TranslationWrite { bone: *id, value, named: true });
            }
            (None, _) => plan.translation = Some(TranslationWrite { bone: *id, value: [0.0; 3], named: false }),
            (Some(_), None) => {}
        },
    }
    plan.missing_in_rig.sort_unstable();
    plan.missing_in_rig.dedup();
    plan.locked.sort_unstable();
    plan.locked.dedup();
    plan
}

const ROTATION: [&str; 3] = ["transform.r.x", "transform.r.y", "transform.r.z"];
const TRANSLATION: [&str; 3] = ["transform.t.x", "transform.t.y", "transform.t.z"];

impl Plan {
    /// Drops every reset (a zero for something the file does not name) of a channel group the parameter does not
    /// drive yet, as told by `owned`: with no binding the value is zero already, and a new binding would make the
    /// parameter a second owner of that channel.
    pub fn drop_unowned_resets(&mut self, owned: impl Fn(u32, &str) -> bool) {
        self.rotations.retain(|write| write.named || !write.identity || ROTATION.iter().any(|c| owned(write.bone, c)));
        self.translation.take_if(|write| !write.named && !TRANSLATION.iter().any(|c| owned(write.bone, c)));
    }

    /// (bone, channel, value) cells to write into one keypoint.
    pub fn cells(&self) -> Vec<(u32, &'static str, f64)> {
        let rotations = self.rotations.iter().map(|write| (write.bone, ROTATION, write.angles));
        let translation = self.translation.iter().map(|write| (write.bone, TRANSLATION, write.value));
        // `+ 0.0` turns -0.0 into 0.0, so the stored value does not depend on which zero was there.
        rotations
            .chain(translation)
            .flat_map(|(bone, channels, values)| channels.into_iter().zip(values).map(move |(c, v)| (bone, c, v + 0.0)))
            .collect()
    }

    pub fn bones(&self) -> Vec<u32> {
        let mut bones: Vec<u32> = self.rotations.iter().map(|write| write.bone).collect();
        bones.extend(self.translation.as_ref().map(|write| write.bone));
        bones.sort_unstable();
        bones.dedup();
        bones
    }

    /// The report returned by the operation. It does not depend on `dryRun` or on the revision.
    pub fn report(&self, pose: &Pose, root: u32, parameter: u32, key: [usize; 2]) -> Value {
        // `mapped`: written from the file (`namedInFile`) or computed to undo a parent. `reset`: not named and at
        // the identity, written as zero so the keypoint does not keep an earlier value.
        let (mut mapped, mut reset) = (Vec::new(), Vec::new());
        for write in &self.rotations {
            if write.named || !write.identity {
                // The names as the file spells them, not the slots an override read them under.
                let sources: Vec<&str> =
                    write.sources.iter().map(|slot| pose.renamed.get(slot).map_or(*slot, String::as_str)).collect();
                mapped.push(json!({
                    "mmd": sources, "rig": write.rig, "bone": write.bone, "channel": "rotation",
                    "pitch": write.angles[0], "yaw": write.angles[1], "roll": write.angles[2],
                    "namedInFile": write.named,
                }));
            } else {
                reset.push(json!({ "rig": write.rig, "bone": write.bone, "channel": "rotation" }));
            }
        }
        if let Some(write) = &self.translation {
            if write.named {
                mapped.push(json!({
                    "mmd": [CENTER], "rig": "Pelvis", "bone": write.bone, "channel": "translation",
                    "value": write.value, "namedInFile": true,
                }));
            } else {
                reset.push(json!({ "rig": "Pelvis", "bone": write.bone, "channel": "translation" }));
            }
        }
        let names = |names: &[super::retarget::UnmappedName]| -> Vec<Value> {
            names
                .iter()
                .take(MAX_REPORTED_NAMES)
                .map(|name| {
                    let text: String = name.text.chars().map(|c| if c.is_control() { '?' } else { c }).collect();
                    json!({ "text": text, "hex": name.hex, "standard": name.standard })
                })
                .collect()
        };
        json!({
            "format": pose.format,
            "frame": pose.frame,
            "root": root,
            "parameter": parameter,
            "key": key,
            "mapped": mapped,
            "reset": reset,
            "unmappedBones": names(&pose.report.unmapped_bones),
            "unmappedBoneCount": pose.report.unmapped_bones.len(),
            "unmappedMorphs": names(&pose.report.unmapped_morphs),
            "unmappedMorphCount": pose.report.unmapped_morphs.len(),
            "missingInRig": self.missing_in_rig,
            "locked": self.locked,
            "zeroQuaternions": pose.report.zero_quaternions,
            "translationApplied": self.translation.as_ref().is_some_and(|write| write.named),
            "translationScale": self.scale,
        })
    }
}
