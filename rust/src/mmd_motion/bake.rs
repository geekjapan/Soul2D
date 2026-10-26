//! Input parsing and the report for `rig.bake_mmd_motion`, which samples a VMD at a list of frames and writes each
//! sampled pose into successive keypoints along one parameter axis. The planning and the keypoint writes are
//! `apply::plan` and `evaluation::set_pose_cells`; `Session::op_bake_mmd_motion` strings them together. See "Baking a
//! motion" in `doc/mmd-motion.md`.

use super::apply::Plan;
use super::retarget::{check_mmd_name, name_label, RetargetReport};
use super::MAX_FRAME;
use serde_json::{json, Value};

/// At most this many frames are sampled by one call (and never more than the parameter has keypoints on the axis).
pub const MAX_BAKE_FRAMES: usize = 256;

/// `frames`: 1 to `MAX_BAKE_FRAMES` strictly increasing whole VMD frame numbers.
pub fn parse_frames(value: &Value) -> Result<Vec<u32>, String> {
    let list = value
        .as_array()
        .filter(|list| (1..=MAX_BAKE_FRAMES).contains(&list.len()))
        .ok_or_else(|| format!("frames must be an array of 1 to {MAX_BAKE_FRAMES} frame numbers"))?;
    let frames = list
        .iter()
        .map(|frame| frame.as_u64().and_then(|frame| u32::try_from(frame).ok()).filter(|frame| *frame <= MAX_FRAME))
        .collect::<Option<Vec<u32>>>()
        .ok_or_else(|| format!("frames must be integers from 0 to {MAX_FRAME}"))?;
    if frames.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err("frames must be strictly increasing".into());
    }
    Ok(frames)
}

/// At most this many entries in the `morphs` mapping.
pub const MAX_MORPH_MAPPINGS: usize = 64;

/// One entry of `morphs`: the morph drives `axis` of `parameter`, its weight 0 to 1 mapped onto `range` (`None`: that
/// axis's minimum to maximum).
pub struct MorphMapping {
    pub morph: String,
    pub parameter: u32,
    pub axis: usize,
    pub range: Option<[f64; 2]>,
}

/// `morphs`: `{"<MMD morph name>": {"parameter": id, "axis": 0 | 1, "range": [at weight 0, at weight 1]}}`. Whether
/// the parameters exist is the caller's check.
pub fn parse_morphs(value: &Value) -> Result<Vec<MorphMapping>, String> {
    let entries = value.as_object().filter(|entries| entries.len() <= MAX_MORPH_MAPPINGS).ok_or_else(|| {
        format!("morphs must be an object of at most {MAX_MORPH_MAPPINGS} entries (MMD morph name to {{parameter, axis, range}})")
    })?;
    fn optional<'a>(entry: &'a Value, key: &str) -> Option<&'a Value> {
        entry.get(key).filter(|value| !value.is_null())
    }
    entries
        .iter()
        .map(|(name, entry)| {
            check_mmd_name("morphs", name)?;
            let label = name_label(name);
            let parameter = entry
                .get("parameter")
                .and_then(Value::as_u64)
                .and_then(crate::valid_id)
                .ok_or_else(|| format!("morphs entry \"{label}\" must be an object with an integer parameter id"))?;
            let axis = match optional(entry, "axis") {
                None => 0,
                Some(axis) => axis
                    .as_u64()
                    .filter(|axis| *axis < 2)
                    .ok_or_else(|| format!("morphs entry \"{label}\": axis must be 0 or 1"))? as usize,
            };
            let range = match optional(entry, "range") {
                None => None,
                // An animation lane stores f32, so each end must be finite as one.
                Some(range) => Some(
                    range
                        .as_array()
                        .filter(|ends| ends.len() == 2)
                        .and_then(|ends| Some([ends[0].as_f64()?, ends[1].as_f64()?]))
                        .filter(|ends| ends.iter().all(|end| (*end as f32).is_finite()))
                        .ok_or_else(|| format!("morphs entry \"{label}\": range must be two finite numbers"))?,
                ),
            };
            Ok(MorphMapping { morph: name.clone(), parameter, axis, range })
        })
        .collect()
}

/// The report returned by the operation. It does not depend on `dryRun` or on the revision. `plans[i]` was written
/// at `keys[i]` for `frames[i]`. File-wide lists are counts only; `rig.apply_mmd_pose` names the unmapped bones.
pub fn report(frames: &[u32], keys: &[[usize; 2]], plans: &[Plan], retarget: &RetargetReport) -> Value {
    let sampled: Vec<Value> = frames
        .iter()
        .zip(keys)
        .zip(plans)
        .map(|((frame, key), plan)| {
            json!({
                "frame": frame,
                "key": key,
                "namedBones": plan.rotations.iter().filter(|write| write.named).count(),
                "translationApplied": plan.translation.as_ref().is_some_and(|write| write.named),
                "locked": plan.locked,
            })
        })
        .collect();
    let union = |names: fn(&Plan) -> &Vec<&'static str>| {
        let mut all: Vec<&str> = plans.iter().flat_map(|plan| names(plan).iter().copied()).collect();
        all.sort_unstable();
        all.dedup();
        all
    };
    json!({
        "frames": sampled,
        "motionFrameRange": retarget.frame_range.map(|(first, last)| [first, last]),
        "unmappedBoneCount": retarget.unmapped_bones.len(),
        "unmappedMorphCount": retarget.unmapped_morphs.len(),
        "missingInRig": union(|plan| &plan.missing_in_rig),
        "locked": union(|plan| &plan.locked),
        "zeroQuaternions": retarget.zero_quaternions,
        "translationScale": plans.first().and_then(|plan| plan.scale),
    })
}
