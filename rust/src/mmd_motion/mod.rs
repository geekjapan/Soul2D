//! MMD motion (VMD) and pose (VPD) readers with retargeting data for the standard depth skeleton.
//!
//! Issue #101. Slice one is the pure readers and retargeting maths; slice two (`apply`) plans a pose for the
//! `rig.apply_mmd_pose` operation; slice three (`bake`) bakes sampled frames into keypoints and an animation lane;
//! slice four adds the bone table override (`retarget::BoneTable`) and the morph lanes of the bake.
//! There is no per-frame playback of the VMD itself. Design notes, limits and what is not done are in
//! `doc/mmd-motion.md`. No real VMD or VPD file was read while writing this: the
//! formats are implemented from their commonly documented layouts and tested on streams built in the tests.

pub mod apply;
pub mod bake;
pub mod retarget;
pub mod sjis;
mod sjis_table;
pub mod vmd;
pub mod vpd;

#[cfg(test)]
mod apply_tests;
#[cfg(test)]
mod tests;

pub use retarget::{
    lookup_bone, mmd_position_to_rig, mmd_quaternion_inverse, mmd_quaternion_mul, mmd_quaternion_to_bone_pose,
    mmd_quaternion_to_rig_world_pitch_yaw_roll, report, rig_world_pitch_yaw_roll_to_mmd_quaternion, BoneMapping,
    BoneTable, Channel, MappedBone, RetargetReport, RigTarget, UnmappedName, BONE_TABLE, MAX_BONE_TABLE_ENTRIES,
    MAX_MMD_NAME_BYTES, MORPH_VOCABULARY,
};
pub use sjis::MmdName;
pub use vmd::{
    bezier_ease, frame_at_seconds, parse_vmd, sample_bone, sample_morph, slerp, BoneKey, BoneTrack, MorphKey,
    SkippedSections, Vmd, FRAMES_PER_SECOND, MAX_FRAME, MAX_VMD_BYTES,
};
pub use vpd::{parse_vpd, Vpd, VpdBone, MAX_VPD_BONES, MAX_VPD_BYTES, MAX_VPD_NAME_BYTES};
