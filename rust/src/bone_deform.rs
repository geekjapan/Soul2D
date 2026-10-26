//! CPU depth-bone offsets for the current key.
//!
//! The formula follows `buildDepthRigRuntime` and the transform-feedback
//! shader in `bone_gpu_async.d`. Results are f64. Comparison with the D
//! float shader is within an absolute tolerance of `1e-4`.
//!
//! Preview stores the offsets outside the model. Apply writes only the armed
//! key of a `"deform"` binding, as an array of `[x, y]` per vertex (D
//! `Deformation.vertexOffsets`). A bound ancestor grid subtracts the inherited
//! offset the way `depthBoneAncestorGridInfluence` does. Camera, Mask,
//! Composite, DynamicComposite, SimplePhysics, and Shapes follow soul2d_live
//! `mustPropagate`. Mesh consumption of those offsets stays fail-closed. Any
//! other intermediate type still fails closed.

use crate::evaluation;
use crate::nodes;
use crate::valid_id;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

const ABSOLUTE_TOLERANCE: f64 = 1e-4;
const MAX_BONES: usize = 64;
const MAX_SOURCES: usize = 128;
const MAX_INFLUENCES: usize = 16;
/// One deform cell may not exceed this many vertices. P5-C1 assembles a cell
/// between the flush sample cap and this limit. A larger cell fails closed.
pub(crate) const MAX_VERTICES: usize = 1_000_000;

#[derive(Clone, Copy)]
struct Quat {
    w: f64,
    x: f64,
    y: f64,
    z: f64,
}

pub(crate) type Mat4 = [[f64; 4]; 4];

struct BoneRuntime {
    id: u32,
    parent_index: Option<usize>,
    rest_head: [f64; 3],
    rest_tail: [f64; 3],
    rest_length: f64,
    rest_quat: Quat,
    local_rest_quat: Quat,
    local_rest_offset: [f64; 3],
    pose_translation: [f64; 3],
    pose_quat: Quat,
    pose_without_yaw: Quat,
    pose_yaw: f64,
    world_head: [f64; 3],
    world_quat: Quat,
    world_prefix: Quat,
    skin: Mat4,
    no_yaw_skin: Mat4,
    allow_parent: bool,
    lock_to_root: bool,
}

pub(crate) struct DeformOutcome {
    pub model: Option<Value>,
    pub body: Value,
}

pub(crate) fn run(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    targets: &[u32],
    parameter: Option<u32>,
    apply: bool,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<DeformOutcome, String> {
    run_with_key(
        model, values, root_id, targets, parameter, apply, preview, None, false,
    )
}

/// `explicit_key` selects one parameter key. `None` uses the closest key of the
/// runtime value, which is what `Preview` and `Apply` do.
pub(crate) fn run_with_key(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    targets: &[u32],
    parameter: Option<u32>,
    apply: bool,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    explicit_key: Option<[usize; 2]>,
    replace_lengths: bool,
) -> Result<DeformOutcome, String> {
    if apply && parameter.is_none() {
        return Err("No armed parameter".into());
    }
    if let Some(parameter) = parameter {
        if !parameter_drives_rig(model, root_id, parameter)? {
            return Err("Armed parameter does not drive this DepthRigRoot".into());
        }
    }
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    let target_ids = resolve_targets(model, root, targets)?;
    if target_ids.is_empty() {
        return Err(if targets.is_empty() {
            "No targets".into()
        } else {
            "No depth bone GPU targets updated".into()
        });
    }
    let keypoint = match explicit_key {
        Some(key) => {
            if parameter.is_none() {
                return Err("No armed parameter".into());
            }
            Some(key)
        }
        None => parameter
            .map(|id| closest_keypoint(model, values, id))
            .transpose()?,
    };
    let capture_packets = apply && crate::gpu_deform::device_present();
    let buffers = deform_buffers(
        model, values, root_id, &target_ids, parameter, keypoint, preview, capture_packets,
    )?;
    if capture_packets {
        // A mismatch or a non-finite GPU buffer is discarded. The CPU offsets
        // below are what the document stores.
        let _ = compare_gpu(
            model,
            &buffers.posed,
            root_id,
            buffers.parameter,
            buffers.keypoint,
            preview,
            &buffers.packets,
            &buffers.offsets,
        );
    }
    let generated = buffers.offsets;
    let keypoint = buffers.keypoint;
    let body = json!({
        "tolerance": ABSOLUTE_TOLERANCE,
        "keypoint": keypoint.map(|key| json!([key[0], key[1]])).unwrap_or(Value::Null),
        "targets": generated.iter().map(|(id, offsets)| json!({
            "target": id,
            "offsets": offsets,
        })).collect::<Vec<_>>(),
    });
    if !apply {
        return Ok(DeformOutcome { model: None, body });
    }
    let mut candidate = model.clone();
    let parameter = parameter.ok_or("No armed parameter")?;
    let keypoint = keypoint.ok_or("No armed parameter")?;
    for (target_id, offsets) in &generated {
        write_deform_key(
            &mut candidate,
            parameter,
            *target_id,
            keypoint,
            offsets,
            replace_lengths,
        )?;
    }
    Ok(DeformOutcome {
        model: Some(candidate),
        body,
    })
}

/// Offsets for vertices `[start, end)` of one cell. Concatenating every range
/// matches `run_with_key` for that target, including ancestor compensation.
pub(crate) fn cell_offset_range(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    target_id: u32,
    parameter: u32,
    keypoint: [usize; 2],
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    start: usize,
    end: usize,
) -> Result<Vec<[f64; 2]>, String> {
    if start > end {
        return Err("offset length mismatch".into());
    }
    let posed = posed_model(model, values)?;
    let bones = build_runtime_limited(model, root_id, Some(parameter), Some(keypoint), false)?;
    let world_scale = depth_world_scale(model, &posed, root_id)?;
    let (raw, _) = offsets_for_target(
        model,
        &posed,
        root_id,
        target_id,
        &bones,
        world_scale,
        false,
        Some(start..end),
    )?;
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    settle_offsets(
        model,
        &posed,
        root,
        target_id,
        Some(parameter),
        Some(keypoint),
        preview,
        &raw,
        start,
        false,
        &BTreeMap::new(),
    )
}

/// Writes one finished cell. A short array is rejected. The software kernel
/// still compares the assembled offsets when a device is present, and a
/// mismatch does not replace them.
pub(crate) fn commit_cell_offsets(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    target_id: u32,
    parameter: u32,
    keypoint: [usize; 2],
    offsets: &[[f64; 2]],
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<DeformOutcome, String> {
    if offsets
        .iter()
        .any(|offset| !offset[0].is_finite() || !offset[1].is_finite())
    {
        return Err("non-finite matrix".into());
    }
    let node = nodes::find_node(model, target_id)
        .ok_or_else(|| format!("node {target_id} was not found"))?;
    if target_vertices(node)?.len() != offsets.len() {
        return Err("offset length mismatch".into());
    }
    if crate::gpu_deform::device_present() {
        let _ = compare_assembled_cell(
            model, values, root_id, target_id, parameter, keypoint, preview, offsets,
        );
    }
    let mut candidate = model.clone();
    write_deform_key(
        &mut candidate,
        parameter,
        target_id,
        keypoint,
        offsets,
        true,
    )?;
    Ok(DeformOutcome {
        model: Some(candidate),
        body: json!({
            "targets": [{
                "target": target_id,
                "offsets": offsets,
            }],
        }),
    })
}

fn compare_assembled_cell(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    target_id: u32,
    parameter: u32,
    keypoint: [usize; 2],
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    offsets: &[[f64; 2]],
) -> Result<f64, String> {
    let buffers = deform_buffers(
        model,
        values,
        root_id,
        &[target_id],
        Some(parameter),
        Some(keypoint),
        preview,
        true,
    )?;
    let mut cpu = buffers.offsets;
    if let Some(entry) = cpu.iter_mut().find(|(id, _)| *id == target_id) {
        entry.1 = offsets.to_vec();
    }
    compare_gpu(
        model,
        &buffers.posed,
        root_id,
        buffers.parameter,
        buffers.keypoint,
        preview,
        &buffers.packets,
        &cpu,
    )
}

struct DeformBuffers {
    posed: Value,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    offsets: Vec<(u32, Vec<[f64; 2]>)>,
    packets: Vec<(u32, crate::gpu_deform::Packet)>,
}

fn deform_buffers(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    target_ids: &[u32],
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    capture_packets: bool,
) -> Result<DeformBuffers, String> {
    let posed = posed_model(model, values)?;
    // The 64-bone cap belongs to the software kernel packet. The CPU offsets
    // are still produced and committed when that packet is rejected.
    let bones = build_runtime_limited(model, root_id, parameter, keypoint, false)?;
    let world_scale = depth_world_scale(model, &posed, root_id)?;
    let mut offsets = Vec::new();
    let mut packets = Vec::new();
    for target_id in target_ids {
        let (target_offsets, packet) = offsets_for_target(
            model, &posed, root_id, *target_id, &bones, world_scale, capture_packets, None,
        )?;
        offsets.push((*target_id, target_offsets));
        if let Some(packet) = packet {
            packets.push((*target_id, packet));
        }
    }
    let offsets = compensate_ancestor_grids(
        model, &posed, root_id, parameter, keypoint, preview, offsets,
    )?;
    Ok(DeformBuffers {
        posed,
        parameter,
        keypoint,
        offsets,
        packets,
    })
}

pub(crate) fn gpu_offsets(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    targets: &[u32],
    parameter: Option<u32>,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<Value, String> {
    if !crate::gpu_deform::device_present() {
        return Ok(json!({
            "device": false,
            "backend": "absent",
            "hardware": false,
        }));
    }
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    if let Some(parameter) = parameter {
        if !parameter_drives_rig(model, root_id, parameter)? {
            return Err("Armed parameter does not drive this DepthRigRoot".into());
        }
    }
    let target_ids = resolve_targets(model, root, targets)?;
    if target_ids.is_empty() {
        return Err(if targets.is_empty() {
            "No targets".into()
        } else {
            "No depth bone GPU targets updated".into()
        });
    }
    let keypoint = parameter
        .map(|id| closest_keypoint(model, values, id))
        .transpose()?;
    let buffers = deform_buffers(
        model, values, root_id, &target_ids, parameter, keypoint, preview, true,
    )?;
    let compared = compare_gpu(
        model,
        &buffers.posed,
        root_id,
        buffers.parameter,
        buffers.keypoint,
        preview,
        &buffers.packets,
        &buffers.offsets,
    );
    let (matched, max_abs, reason) = match compared {
        Ok(max_abs) => (true, Some(max_abs), Value::Null),
        Err(error) => (false, None, Value::String(error)),
    };
    Ok(json!({
        "device": true,
        "backend": crate::gpu_deform::backend(),
        "hardware": crate::gpu_deform::hardware(),
        "matched": matched,
        "maxAbs": max_abs,
        "reason": reason,
        "targets": buffers.offsets.iter().map(|(id, offsets)| json!({
            "target": id,
            "offsets": offsets,
        })).collect::<Vec<_>>(),
    }))
}

fn posed_model(model: &Value, values: &BTreeMap<u32, [f64; 2]>) -> Result<Value, String> {
    let has_parameters = model
        .get("param")
        .and_then(Value::as_array)
        .is_some_and(|items| !items.is_empty());
    if !has_parameters && values.is_empty() {
        return Ok(model.clone());
    }
    evaluation::evaluate(model, values)
}

fn resolve_targets(model: &Value, root: &Value, requested: &[u32]) -> Result<Vec<u32>, String> {
    let mut ids = Vec::new();
    if requested.is_empty() {
        let Some(bindings) = root.get("bindings").and_then(Value::as_array) else {
            return Ok(ids);
        };
        for binding in bindings {
            let Some(id) = binding
                .get("target")
                .and_then(Value::as_u64)
                .and_then(valid_id)
            else {
                continue;
            };
            if nodes::find_node(model, id).is_some() {
                ids.push(id);
            }
        }
        return Ok(ids);
    }
    for id in requested {
        let node =
            nodes::find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        if !matches!(
            node.get("type").and_then(Value::as_str),
            Some("GridDeformer" | "PathDeformer" | "MeshGroup" | "Part")
        ) {
            return Err("target is not deformable".into());
        }
        if binding_for(root, *id).is_some() {
            ids.push(*id);
        }
    }
    Ok(ids)
}

fn compensate_ancestor_grids(
    model: &Value,
    posed: &Value,
    root_id: u32,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    generated: Vec<(u32, Vec<[f64; 2]>)>,
) -> Result<Vec<(u32, Vec<[f64; 2]>)>, String> {
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let request_order: Vec<u32> = generated.iter().map(|(id, _)| *id).collect();
    let mut order = request_order.clone();
    order.sort_by_key(|id| hierarchy_depth(model, *id).unwrap_or(0));
    let by_id: BTreeMap<u32, Vec<[f64; 2]>> = generated.into_iter().collect();
    let mut resolved = BTreeMap::new();
    for target_id in order {
        let original = by_id.get(&target_id).cloned().unwrap_or_default();
        let adjusted = settle_offsets(
            model, posed, root, target_id, parameter, keypoint, preview, &original, 0, true,
            &resolved,
        )?;
        resolved.insert(target_id, adjusted);
    }
    Ok(request_order
        .into_iter()
        .filter_map(|id| resolved.remove(&id).map(|offsets| (id, offsets)))
        .collect())
}

fn hierarchy_depth(model: &Value, id: u32) -> Result<usize, String> {
    let mut depth = 0usize;
    let mut cursor = Some(id);
    while let Some(current) = cursor {
        depth += 1;
        cursor = parent_of(model, current)?;
    }
    Ok(depth)
}

fn settle_offsets(
    model: &Value,
    posed: &Value,
    root: &Value,
    target_id: u32,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    original: &[[f64; 2]],
    index_base: usize,
    full_buffer: bool,
    resolved: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<Vec<[f64; 2]>, String> {
    let mut adjusted = original.to_vec();
    for _ in 0..6 {
        let inherited = ancestor_influence(
            model,
            posed,
            root,
            target_id,
            parameter,
            keypoint,
            preview,
            &adjusted,
            resolved,
            index_base,
            full_buffer,
        )?;
        if inherited.len() != adjusted.len() {
            break;
        }
        let next: Vec<[f64; 2]> = original
            .iter()
            .zip(inherited.iter())
            .map(|(base, inherited)| [base[0] - inherited[0], base[1] - inherited[1]])
            .collect();
        let converged = next.iter().zip(adjusted.iter()).all(|(left, right)| {
            (left[0] - right[0]).abs() <= 1e-4 && (left[1] - right[1]).abs() <= 1e-4
        });
        adjusted = next;
        if converged {
            break;
        }
    }
    if adjusted
        .iter()
        .any(|offset| !offset[0].is_finite() || !offset[1].is_finite())
    {
        return Err("non-finite matrix".into());
    }
    Ok(adjusted)
}

fn ancestor_influence(
    model: &Value,
    posed: &Value,
    root: &Value,
    target_id: u32,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    adjusted: &[[f64; 2]],
    resolved: &BTreeMap<u32, Vec<[f64; 2]>>,
    index_base: usize,
    full_buffer: bool,
) -> Result<Vec<[f64; 2]>, String> {
    let mut result = adjusted.to_vec();
    let original = result.clone();
    let mut ancestors = Vec::new();
    let mut cursor = parent_of(model, target_id)?;
    while let Some(parent_id) = cursor {
        let node = nodes::find_node(model, parent_id)
            .ok_or_else(|| format!("node {parent_id} was not found"))?;
        if node.get("type").and_then(Value::as_str) == Some("GridDeformer") {
            ancestors.push(parent_id);
        }
        cursor = parent_of(model, parent_id)?;
    }
    for ancestor in ancestors.into_iter().rev() {
        if binding_for(root, ancestor).is_none() {
            continue;
        }
        if !influence_can_reach(model, root, ancestor, target_id)? {
            continue;
        }
        let Some(stopper) = propagation_stopper(model, ancestor, target_id)? else {
            continue;
        };
        let Some(parent_offsets) =
            stored_parent_offsets(model, ancestor, parameter, keypoint, preview, resolved)?
        else {
            continue;
        };
        if stopper.is_none() {
            add_parent_grid_influence(
                model,
                posed,
                ancestor,
                target_id,
                &parent_offsets,
                &mut result,
                index_base,
                full_buffer,
            )?;
        } else if let Some(stopper_id) = stopper {
            add_parent_origin_influence(
                model,
                posed,
                ancestor,
                stopper_id,
                target_id,
                &parent_offsets,
                &mut result,
            )?;
        }
    }
    Ok(result
        .into_iter()
        .zip(original)
        .map(|(value, base)| [value[0] - base[0], value[1] - base[1]])
        .collect())
}

fn stored_parent_offsets(
    model: &Value,
    parent_id: u32,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    resolved: &BTreeMap<u32, Vec<[f64; 2]>>,
) -> Result<Option<Vec<[f64; 2]>>, String> {
    if let Some(offsets) = resolved.get(&parent_id) {
        return Ok(Some(offsets.clone()));
    }
    let parent = nodes::find_node(model, parent_id)
        .ok_or_else(|| format!("node {parent_id} was not found"))?;
    let vertices = target_vertices(parent)?;
    if let (Some(parameter), Some(keypoint)) = (parameter, keypoint) {
        if let Some(cell) = deform_offsets_at(model, parameter, parent_id, keypoint)? {
            if cell.len() == vertices.len() {
                return Ok(Some(cell));
            }
        }
    } else if let Some(offsets) = preview.get(&parent_id) {
        if offsets.len() == vertices.len() {
            return Ok(Some(offsets.clone()));
        }
    }
    Ok(None)
}

fn deform_offsets_at(
    model: &Value,
    parameter_id: u32,
    node_id: u32,
    keypoint: [usize; 2],
) -> Result<Option<Vec<[f64; 2]>>, String> {
    let Some(parameter) = find_parameter(model, parameter_id) else {
        return Ok(None);
    };
    let Some(bindings) = parameter.get("bindings").and_then(Value::as_array) else {
        return Ok(None);
    };
    let Some(binding) = bindings.iter().find(|binding| {
        binding
            .get("node")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            == Some(node_id)
            && binding.get("param_name").and_then(Value::as_str) == Some("deform")
    }) else {
        return Ok(None);
    };
    let Some(column) = binding
        .get("values")
        .and_then(Value::as_array)
        .and_then(|columns| columns.get(keypoint[0]))
        .and_then(Value::as_array)
    else {
        return Ok(None);
    };
    let Some(cell) = column.get(keypoint[1]).and_then(Value::as_array) else {
        return Ok(None);
    };
    let mut offsets = Vec::new();
    for point in cell {
        let pair = point.as_array().ok_or("offset length mismatch")?;
        if pair.len() < 2 {
            return Err("offset length mismatch".into());
        }
        let x = pair[0]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or("non-finite matrix")?;
        let y = pair[1]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or("non-finite matrix")?;
        offsets.push([x, y]);
    }
    Ok(Some(offsets))
}

fn influence_can_reach(
    model: &Value,
    root: &Value,
    parent_grid: u32,
    target_id: u32,
) -> Result<bool, String> {
    let Some(parent_binding) = binding_for(root, parent_grid) else {
        return Ok(false);
    };
    let Some(target_binding) = binding_for(root, target_id) else {
        return Ok(false);
    };
    let parent_sources = source_ids(parent_binding)?;
    let target_sources = source_ids(target_binding)?;
    for parent_source in &parent_sources {
        for target_source in &target_sources {
            if source_includes_ancestor(model, *target_source, *parent_source)? {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn source_ids(binding: &Value) -> Result<Vec<u32>, String> {
    let Some(raw) = binding.get("sourceBoneUuids").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    raw.iter()
        .map(|value| {
            value
                .as_u64()
                .and_then(valid_id)
                .ok_or_else(|| "sourceBoneUuids must contain node UUIDs".to_string())
        })
        .collect()
}

fn source_includes_ancestor(
    model: &Value,
    target_bone: u32,
    ancestor_bone: u32,
) -> Result<bool, String> {
    let mut cursor = Some(target_bone);
    while let Some(id) = cursor {
        if id == ancestor_bone {
            return Ok(true);
        }
        let node = nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
        let allow = node
            .get("allowParentToTargets")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        if !allow || flag(node, "lockToRoot") {
            return Ok(false);
        }
        cursor = parent_of(model, id)?.filter(|parent| {
            nodes::find_node(model, *parent)
                .and_then(|node| node.get("type"))
                .and_then(Value::as_str)
                == Some("DepthBone")
        });
    }
    Ok(false)
}

fn propagation_stopper(
    model: &Value,
    ancestor: u32,
    target: u32,
) -> Result<Option<Option<u32>>, String> {
    let mut intermediates = Vec::new();
    let mut cursor = parent_of(model, target)?;
    while let Some(id) = cursor {
        if id == ancestor {
            for intermediate in intermediates.iter().rev() {
                let node = nodes::find_node(model, *intermediate)
                    .ok_or_else(|| format!("node {intermediate} was not found"))?;
                if !must_propagate(node)? {
                    return Ok(Some(Some(*intermediate)));
                }
            }
            return Ok(Some(None));
        }
        intermediates.push(id);
        cursor = parent_of(model, id)?;
    }
    Ok(None)
}

fn must_propagate(node: &Value) -> Result<bool, String> {
    // soul2d_live overrides. `false` is a `gridPropagationStopper` stopper.
    match node.get("type").and_then(Value::as_str).unwrap_or("") {
        "GridDeformer" | "Node" | "MeshGroup" | "DepthBone" | "DepthRigRoot" => Ok(false),
        // Camera, SimplePhysics, and Shapes extend Node. mustPropagate is false.
        // They are not Deformable, so a stopper can still add origin influence.
        "Camera" | "SimplePhysics" | "Shapes" => Ok(false),
        "PathDeformer" | "Part" => Ok(true),
        // Mask extends Part, which extends Drawable. Drawable.mustPropagate is true.
        "Mask" => Ok(true),
        // Composite.mustPropagate is propagateMeshGroup. A missing key stays true.
        "Composite" => Ok(flag_default(node, "propagate_meshgroup", true)),
        // DynamicComposite extends Projectable. mustPropagate is false even
        // when a stray propagate_meshgroup is present.
        "DynamicComposite" => Ok(false),
        other => Err(format!(
            "cannot match D ancestor grid propagation for {other}"
        )),
    }
}

fn flag_default(node: &Value, key: &str, default: bool) -> bool {
    node.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn add_parent_grid_influence(
    model: &Value,
    posed: &Value,
    parent_id: u32,
    target_id: u32,
    parent_offsets: &[[f64; 2]],
    accumulated: &mut [[f64; 2]],
    index_base: usize,
    full_buffer: bool,
) -> Result<(), String> {
    let parent = nodes::find_node(model, parent_id)
        .ok_or_else(|| format!("node {parent_id} was not found"))?;
    let target = nodes::find_node(model, target_id)
        .ok_or_else(|| format!("node {target_id} was not found"))?;
    let parent_vertices = target_vertices(parent)?;
    let target_vertices = target_vertices(target)?;
    let end = index_base.saturating_add(accumulated.len());
    if parent_offsets.len() != parent_vertices.len() || end > target_vertices.len() {
        return Ok(());
    }
    // A full-cell caller that does not cover every vertex keeps the previous
    // skip. A chunk passes `full_buffer = false` and samples only its range.
    if full_buffer && accumulated.len() != target_vertices.len() {
        return Ok(());
    }
    let Some((axis_x, axis_y)) = sampling_axes(&parent_vertices) else {
        return Ok(());
    };
    let dynamic = parent
        .get("dynamic")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let to_parent = relative_matrix(posed, parent_id, target_id)?;
    let to_target = invert(to_parent)?;
    for (local, slot) in accumulated.iter_mut().enumerate() {
        let vertex = target_vertices[index_base + local];
        let mut parent_point = transform_point(to_parent, [vertex[0], vertex[1], 0.0])?;
        if dynamic {
            let prior = transform_vector(to_parent, [slot[0], slot[1], 0.0])?;
            parent_point = add(parent_point, prior);
        }
        let Some(sampled) = sample_grid_offset(
            &parent_vertices,
            parent_offsets,
            &axis_x,
            &axis_y,
            [parent_point[0], parent_point[1]],
        ) else {
            continue;
        };
        let local_offset = transform_vector(to_target, [sampled[0], sampled[1], 0.0])?;
        slot[0] += local_offset[0];
        slot[1] += local_offset[1];
    }
    Ok(())
}

fn add_parent_origin_influence(
    model: &Value,
    posed: &Value,
    parent_id: u32,
    stopper_id: u32,
    target_id: u32,
    parent_offsets: &[[f64; 2]],
    accumulated: &mut [[f64; 2]],
) -> Result<(), String> {
    let parent = nodes::find_node(model, parent_id)
        .ok_or_else(|| format!("node {parent_id} was not found"))?;
    let stopper = nodes::find_node(model, stopper_id)
        .ok_or_else(|| format!("node {stopper_id} was not found"))?;
    // D skips origin influence when `cast(Deformable)stopper !is null`.
    // Camera is a Node stopper, so it can shift the child. Composite and
    // DynamicComposite are Deformable stoppers, so a false propagate flag
    // blocks the ancestor without sampling the stopper origin.
    if matches!(
        stopper.get("type").and_then(Value::as_str),
        Some(
            "GridDeformer"
                | "PathDeformer"
                | "MeshGroup"
                | "Part"
                | "Composite"
                | "DynamicComposite"
        )
    ) {
        return Ok(());
    }
    let translate_children = parent
        .get("translateChildren")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    if !translate_children {
        return Ok(());
    }
    let parent_vertices = target_vertices(parent)?;
    if parent_offsets.len() != parent_vertices.len() || accumulated.is_empty() {
        return Ok(());
    }
    let Some((axis_x, axis_y)) = sampling_axes(&parent_vertices) else {
        return Ok(());
    };
    let stopper_parent = parent_of(model, stopper_id)?;
    let parent_matrix = if let Some(stopper_parent) = stopper_parent {
        world_matrix(posed, stopper_parent)?
    } else {
        identity()
    };
    let to_grid = mul_mat(invert(world_matrix(posed, parent_id)?)?, parent_matrix);
    let local = transform_vec(stopper, "trans", [0.0, 0.0, 0.0]);
    let mut origin = transform_point(to_grid, [local[0], local[1], 0.0])?;
    if parent
        .get("dynamic")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        let posed_stopper = nodes::find_node(posed, stopper_id)
            .ok_or_else(|| format!("node {stopper_id} was not found"))?;
        let posed_trans = transform_vec(posed_stopper, "trans", [0.0, 0.0, 0.0]);
        let offset = transform_vector(
            to_grid,
            [posed_trans[0] - local[0], posed_trans[1] - local[1], 0.0],
        )?;
        origin = add(origin, offset);
    }
    let Some(sampled) = sample_grid_offset(
        &parent_vertices,
        parent_offsets,
        &axis_x,
        &axis_y,
        [origin[0], origin[1]],
    ) else {
        return Ok(());
    };
    let to_target = mul_mat(
        invert(world_matrix(posed, target_id)?)?,
        world_matrix(posed, parent_id)?,
    );
    let local_offset = transform_vector(to_target, [sampled[0], sampled[1], 0.0])?;
    for slot in accumulated.iter_mut() {
        slot[0] += local_offset[0];
        slot[1] += local_offset[1];
    }
    Ok(())
}

fn sampling_axes(vertices: &[[f64; 2]]) -> Option<(Vec<f64>, Vec<f64>)> {
    let mut axis_x = Vec::new();
    let mut axis_y = Vec::new();
    for vertex in vertices {
        push_unique(&mut axis_x, vertex[0]);
        push_unique(&mut axis_y, vertex[1]);
    }
    axis_x.sort_by(|a, b| a.total_cmp(b));
    axis_y.sort_by(|a, b| a.total_cmp(b));
    if axis_x.len() < 2 || axis_y.len() < 2 || axis_x.len() * axis_y.len() != vertices.len() {
        return None;
    }
    Some((axis_x, axis_y))
}

fn push_unique(axis: &mut Vec<f64>, value: f64) {
    if axis.iter().any(|existing| (existing - value).abs() <= 1e-4) {
        return;
    }
    axis.push(value);
}

fn sample_grid_offset(
    vertices: &[[f64; 2]],
    offsets: &[[f64; 2]],
    axis_x: &[f64],
    axis_y: &[f64],
    point: [f64; 2],
) -> Option<[f64; 2]> {
    if offsets.len() != vertices.len() || axis_x.len() < 2 || axis_y.len() < 2 {
        return None;
    }
    let x = point[0].clamp(axis_x[0], *axis_x.last()?);
    let y = point[1].clamp(axis_y[0], *axis_y.last()?);
    let xi = axis_interval(axis_x, x);
    let yi = axis_interval(axis_y, y);
    let x_span = axis_x[xi + 1] - axis_x[xi];
    let y_span = axis_y[yi + 1] - axis_y[yi];
    let u = if x_span > 0.0 {
        (x - axis_x[xi]) / x_span
    } else {
        0.0
    };
    let v = if y_span > 0.0 {
        (y - axis_y[yi]) / y_span
    } else {
        0.0
    };
    let i00 = grid_vertex_index(vertices, axis_x[xi], axis_y[yi])?;
    let i10 = grid_vertex_index(vertices, axis_x[xi + 1], axis_y[yi])?;
    let i01 = grid_vertex_index(vertices, axis_x[xi], axis_y[yi + 1])?;
    let i11 = grid_vertex_index(vertices, axis_x[xi + 1], axis_y[yi + 1])?;
    Some([
        offsets[i00][0] * ((1.0 - u) * (1.0 - v))
            + offsets[i10][0] * (u * (1.0 - v))
            + offsets[i01][0] * ((1.0 - u) * v)
            + offsets[i11][0] * (u * v),
        offsets[i00][1] * ((1.0 - u) * (1.0 - v))
            + offsets[i10][1] * (u * (1.0 - v))
            + offsets[i01][1] * ((1.0 - u) * v)
            + offsets[i11][1] * (u * v),
    ])
}

fn axis_interval(axis: &[f64], value: f64) -> usize {
    if axis.len() < 2 {
        return 0;
    }
    for index in 0..axis.len() - 1 {
        if value <= axis[index + 1] {
            return index;
        }
    }
    axis.len() - 2
}

fn grid_vertex_index(vertices: &[[f64; 2]], x: f64, y: f64) -> Option<usize> {
    vertices
        .iter()
        .position(|vertex| (vertex[0] - x).abs() <= 1e-4 && (vertex[1] - y).abs() <= 1e-4)
}

fn relative_matrix(posed: &Value, parent_id: u32, target_id: u32) -> Result<Mat4, String> {
    Ok(mul_mat(
        invert(world_matrix(posed, parent_id)?)?,
        world_matrix(posed, target_id)?,
    ))
}

fn transform_vector(matrix: Mat4, vector: [f64; 3]) -> Result<[f64; 3], String> {
    let mut output = [0.0; 3];
    for row in 0..3 {
        output[row] =
            matrix[row][0] * vector[0] + matrix[row][1] * vector[1] + matrix[row][2] * vector[2];
    }
    if output.iter().any(|value| !value.is_finite()) {
        return Err("non-finite matrix".into());
    }
    Ok(output)
}

pub(crate) fn parameter_drives_rig(
    model: &Value,
    root_id: u32,
    parameter_id: u32,
) -> Result<bool, String> {
    let parameter = find_parameter(model, parameter_id)
        .ok_or_else(|| format!("parameter {parameter_id} was not found"))?;
    let Some(bindings) = parameter.get("bindings").and_then(Value::as_array) else {
        return Ok(false);
    };
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    for binding in bindings {
        let name = binding
            .get("param_name")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !is_transform_channel(name) {
            continue;
        }
        let Some(node_id) = binding
            .get("node")
            .and_then(Value::as_u64)
            .and_then(valid_id)
        else {
            continue;
        };
        let Some(node) = nodes::find_node(model, node_id) else {
            continue;
        };
        if node.get("type").and_then(Value::as_str) == Some("DepthBone")
            && contains_node(root, node_id)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

fn is_transform_channel(name: &str) -> bool {
    name.starts_with("transform.t.")
        || name.starts_with("transform.r.")
        || name.starts_with("transform.s.")
}

pub(crate) fn closest_keypoint(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    parameter_id: u32,
) -> Result<[usize; 2], String> {
    let parameter = find_parameter(model, parameter_id)
        .ok_or_else(|| format!("parameter {parameter_id} was not found"))?;
    let min = pair(parameter.get("min"), [0.0, 0.0], "parameter min")?;
    let max = pair(parameter.get("max"), [1.0, 1.0], "parameter max")?;
    let defaults = pair(parameter.get("defaults"), [0.0, 0.0], "parameter defaults")?;
    let value = values.get(&parameter_id).copied().unwrap_or(defaults);
    let axes = parameter
        .get("axis_points")
        .and_then(Value::as_array)
        .ok_or("parameter.axis_points must contain two arrays")?;
    if axes.len() != 2 {
        return Err("parameter.axis_points must contain two arrays".into());
    }
    let x_axis = axis_values(&axes[0])?;
    let y_axis = axis_values(&axes[1])?;
    let mapped = [
        map_axis(value[0], min[0], max[0], crate::evaluation::parameter_repeats(parameter))?,
        map_axis(value[1], min[1], max[1], false)?,
    ];
    Ok([
        closest_index(&x_axis, mapped[0]),
        closest_index(&y_axis, mapped[1]),
    ])
}

fn map_axis(value: f64, min: f64, max: f64, repeats: bool) -> Result<f64, String> {
    if max == min {
        return Ok(0.0);
    }
    // The same rule as evaluation, so a rig and the bindings beside it pick the same point of a repeating axis.
    Ok(if repeats {
        crate::evaluation::repeat_position(value, min, max)
    } else {
        ((value - min) / (max - min)).clamp(0.0, 1.0)
    })
}

fn closest_index(axis: &[f64], offset: f64) -> usize {
    let mut best = 0;
    let mut best_dist = f64::INFINITY;
    for (index, point) in axis.iter().enumerate() {
        let dist = (point - offset).abs();
        if dist < best_dist {
            best = index;
            best_dist = dist;
        }
    }
    best
}

fn axis_values(value: &Value) -> Result<Vec<f64>, String> {
    let array = value.as_array().ok_or("parameter axis must be an array")?;
    array
        .iter()
        .map(|item| {
            item.as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| "parameter axis must be finite".to_string())
        })
        .collect()
}

fn build_runtime(
    model: &Value,
    root_id: u32,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
) -> Result<Vec<BoneRuntime>, String> {
    build_runtime_limited(model, root_id, parameter, keypoint, true)
}

fn build_runtime_limited(
    model: &Value,
    root_id: u32,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    limit_bones: bool,
) -> Result<Vec<BoneRuntime>, String> {
    let order = bone_order(model, root_id)?;
    // The 64-bone cap is the GPU offset packet. Overlay reads the same runtime
    // D builds without that cap (`buildDepthRigRuntime`).
    if limit_bones && order.len() > MAX_BONES {
        return Err("Depth bone GPU packet exceeds maximum bone count".into());
    }
    let mut bones: Vec<BoneRuntime> = Vec::with_capacity(order.len());
    for id in &order {
        let node =
            nodes::find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        let (head, tail) = effective_rest(model, root_id, *id)?;
        let mut length = distance(head, tail);
        if length <= 1e-4 {
            length = 1e-4;
        }
        let roll = number_field(node, "restRoll", 0.0);
        let rest_quat = rest_quat(head, tail, roll);
        let parent_index =
            parent_of(model, *id)?.and_then(|parent| order.iter().position(|id| *id == parent));
        let lock_rotation = flag(node, "lockRotation");
        let lock_translation = flag(node, "lockTranslation");
        let (pose_translation, pose_yaw, pose_quat, pose_without_yaw) = pose_of(
            model,
            node,
            parameter,
            keypoint,
            head,
            parent_index.map(|index| bones[index].rest_head),
            lock_rotation,
            lock_translation,
        )?;
        bones.push(BoneRuntime {
            id: *id,
            parent_index,
            rest_head: head,
            rest_tail: tail,
            rest_length: length,
            rest_quat,
            local_rest_quat: rest_quat,
            local_rest_offset: head,
            pose_translation,
            pose_quat,
            pose_without_yaw,
            pose_yaw,
            world_head: [0.0; 3],
            world_quat: rest_quat,
            world_prefix: rest_quat,
            skin: identity(),
            no_yaw_skin: identity(),
            allow_parent: node
                .get("allowParentToTargets")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            lock_to_root: flag(node, "lockToRoot"),
        });
    }
    for index in 0..bones.len() {
        if let Some(parent) = bones[index].parent_index {
            let parent_quat = bones[parent].rest_quat;
            let offset = sub(bones[index].rest_head, bones[parent].rest_head);
            let local_quat = mul_quat(inverse_quat(parent_quat), bones[index].rest_quat);
            let local_offset = rotate_vec(inverse_quat(parent_quat), offset);
            bones[index].local_rest_quat = local_quat;
            bones[index].local_rest_offset = local_offset;
        }
    }
    for index in 0..bones.len() {
        let (world_head, world_prefix) = if bones[index].parent_index.is_some()
            && bones[index].allow_parent
            && !bones[index].lock_to_root
        {
            let parent = bones[index].parent_index.unwrap();
            let parent_quat = bones[parent].world_quat;
            let head = add(
                bones[parent].world_head,
                add(
                    rotate_vec(parent_quat, bones[index].local_rest_offset),
                    rotate_vec(parent_quat, bones[index].pose_translation),
                ),
            );
            (head, mul_quat(parent_quat, bones[index].local_rest_quat))
        } else {
            (
                add(bones[index].rest_head, bones[index].pose_translation),
                bones[index].rest_quat,
            )
        };
        let world_quat = mul_quat(world_prefix, bones[index].pose_quat);
        let bind = compose(bones[index].rest_head, bones[index].rest_quat);
        let inverse_bind = invert(bind)?;
        let skin = mul_mat(compose(world_head, world_quat), inverse_bind);
        let no_yaw = if bones[index].pose_yaw == 0.0 {
            skin
        } else {
            let source_world = mul_quat(world_prefix, bones[index].pose_without_yaw);
            mul_mat(compose(world_head, source_world), inverse_bind)
        };
        if !matrix_finite(skin) || !matrix_finite(no_yaw) {
            return Err("non-finite matrix".into());
        }
        bones[index].world_head = world_head;
        bones[index].world_quat = world_quat;
        bones[index].world_prefix = world_prefix;
        bones[index].skin = skin;
        bones[index].no_yaw_skin = no_yaw;
    }
    Ok(bones)
}

fn pose_of(
    model: &Value,
    node: &Value,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    rest_head: [f64; 3],
    parent_rest: Option<[f64; 3]>,
    lock_rotation: bool,
    lock_translation: bool,
) -> Result<([f64; 3], f64, Quat, Quat), String> {
    if let (Some(parameter), Some(keypoint)) = (parameter, keypoint) {
        let id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .unwrap_or(0);
        let channel = |name: &str| binding_scalar(model, parameter, id, name, keypoint);
        let translation = if lock_translation {
            [0.0; 3]
        } else {
            [
                channel("transform.t.x")?,
                channel("transform.t.y")?,
                channel("transform.t.z")?,
            ]
        };
        let pitch = channel("transform.r.x")?;
        let yaw = channel("transform.r.y")?;
        let roll = channel("transform.r.z")?;
        if lock_rotation {
            return Ok((translation, 0.0, identity_quat(), identity_quat()));
        }
        return Ok((
            translation,
            yaw,
            depth_edit_rotation(pitch, yaw, roll),
            depth_edit_rotation(pitch, 0.0, roll),
        ));
    }
    let local = transform_vec(node, "trans", [0.0, 0.0, 0.0]);
    let rotation = transform_vec(node, "rot", [0.0, 0.0, 0.0]);
    let rest_local = match parent_rest {
        Some(parent) => sub(rest_head, parent),
        None => rest_head,
    };
    let translation = if lock_translation {
        [0.0; 3]
    } else {
        sub(local, rest_local)
    };
    if lock_rotation {
        return Ok((translation, 0.0, identity_quat(), identity_quat()));
    }
    Ok((
        translation,
        rotation[1],
        depth_edit_rotation(rotation[0], rotation[1], rotation[2]),
        depth_edit_rotation(rotation[0], 0.0, rotation[2]),
    ))
}

fn binding_scalar(
    model: &Value,
    parameter_id: u32,
    node_id: u32,
    channel: &str,
    keypoint: [usize; 2],
) -> Result<f64, String> {
    let parameter = find_parameter(model, parameter_id)
        .ok_or_else(|| format!("parameter {parameter_id} was not found"))?;
    let Some(bindings) = parameter.get("bindings").and_then(Value::as_array) else {
        return Ok(0.0);
    };
    let Some(binding) = bindings.iter().find(|binding| {
        binding
            .get("node")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            == Some(node_id)
            && binding.get("param_name").and_then(Value::as_str) == Some(channel)
    }) else {
        return Ok(0.0);
    };
    let Some(values) = binding.get("values").and_then(Value::as_array) else {
        return Ok(0.0);
    };
    let Some(column) = values.get(keypoint[0]).and_then(Value::as_array) else {
        return Ok(0.0);
    };
    let Some(cell) = column.get(keypoint[1]) else {
        return Ok(0.0);
    };
    cell.as_f64()
        .filter(|value| value.is_finite())
        .ok_or_else(|| {
            format!("parameter {parameter_id} binding channel {channel:?} is not a scalar")
        })
}

fn offsets_for_target(
    model: &Value,
    posed: &Value,
    root_id: u32,
    target_id: u32,
    bones: &[BoneRuntime],
    world_scale: f64,
    capture_packet: bool,
    range: Option<Range<usize>>,
) -> Result<(Vec<[f64; 2]>, Option<crate::gpu_deform::Packet>), String> {
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let binding = binding_for(root, target_id).ok_or("Depth rig binding is required")?;
    let target = nodes::find_node(model, target_id)
        .ok_or_else(|| format!("node {target_id} was not found"))?;
    if !is_binding_target(target) {
        return Err("target is not deformable".into());
    }
    let vertices = target_vertices(target)?;
    if vertices.len() > MAX_VERTICES {
        return Err("Depth bone GPU packet exceeds maximum vertex count".into());
    }
    let sources = source_records(binding, bones)?;
    if sources.is_empty() {
        return Err("No valid depth bone sources".into());
    }
    if sources.len() > MAX_SOURCES {
        return Err("Depth bone GPU packet exceeds maximum source count".into());
    }
    let rule = read_rule(binding.get("influenceRule"))?;
    if rule.max_influences > MAX_INFLUENCES as u32 {
        return Err("Depth bone GPU packet exceeds maximum influence count".into());
    }
    let raw_depths = snapshot_depths(target, vertices.len())?;
    let target_to_root = target_to_root(model, posed, root_id, target_id)?;
    let root_to_target = invert(target_to_root)?;
    let scaled: Vec<f64> = raw_depths.iter().map(|depth| depth * world_scale).collect();
    let mut prepared = Vec::new();
    for source in &sources {
        let bone = &bones[source.bone_index];
        let shift = rotation_pivot_shift(
            &vertices,
            &scaled,
            target_to_root,
            bone.rest_head,
            source,
            world_scale,
        )?;
        prepared.push(PreparedSource {
            bone_index: source.bone_index,
            terminal: source.terminal,
            lock_to_root: bone.lock_to_root,
            linear: rule.falloff == "linear",
            weight: source.weight,
            depth_scale: source.depth_scale,
            depth_offset: source.depth_offset * world_scale,
            multiplier: source.multiplier,
            sin_rotation: source.sin_rotation,
            cos_rotation: source.cos_rotation,
            pivot_shift: shift,
            pose_yaw: bone.pose_yaw,
        });
    }
    let radius_floor = rule
        .minimum_radius
        .max(target_bounds_size(&vertices) * 0.18);
    let persisted = persisted_weight_rows(binding, &sources, bones, vertices.len())?;
    let full_range = range.is_none();
    let (start, end) = match range {
        Some(range) => {
            if range.start > range.end || range.end > vertices.len() {
                return Err("offset length mismatch".into());
            }
            (range.start, range.end)
        }
        None => (0, vertices.len()),
    };
    let mut offsets = Vec::with_capacity(end - start);
    for (index, vertex) in vertices.iter().enumerate().take(end).skip(start) {
        let z = scaled.get(index).copied().unwrap_or(0.0);
        let offset = if let Some(weights) = &persisted {
            deform_vertex_weighted(
                *vertex,
                z,
                &prepared,
                bones,
                weights,
                index,
                target_to_root,
                root_to_target,
            )?
        } else {
            deform_vertex(
                *vertex,
                z,
                &prepared,
                bones,
                rule.max_influences as usize,
                rule.radius_scale,
                radius_floor,
                target_to_root,
                root_to_target,
            )?
        };
        offsets.push(offset)
    }
    let packet = if capture_packet && full_range {
        Some(skin_packet(
            &bones,
            &prepared,
            &vertices,
            &scaled,
            persisted.as_ref(),
            target_to_root,
            root_to_target,
            rule.max_influences as usize,
            rule.radius_scale,
            radius_floor,
        ))
    } else {
        None
    };
    Ok((offsets, packet))
}

fn skin_packet(
    bones: &[BoneRuntime],
    sources: &[PreparedSource],
    vertices: &[[f64; 2]],
    depths: &[f64],
    weights: Option<&Vec<Vec<f64>>>,
    target_to_root: Mat4,
    root_to_target: Mat4,
    max_influences: usize,
    radius_scale: f64,
    radius_floor: f64,
) -> crate::gpu_deform::Packet {
    crate::gpu_deform::Packet {
        vertices: vertices.to_vec(),
        depths: depths.to_vec(),
        bones: bones
            .iter()
            .map(|bone| crate::gpu_deform::GpuBone {
                rest_head: bone.rest_head,
                rest_tail: bone.rest_tail,
                rest_length: bone.rest_length,
                parent: bone.parent_index,
                skin: bone.skin,
                no_yaw_skin: bone.no_yaw_skin,
            })
            .collect(),
        sources: sources
            .iter()
            .map(|source| crate::gpu_deform::GpuSource {
                bone: source.bone_index,
                terminal: source.terminal,
                lock_to_root: source.lock_to_root,
                linear: source.linear,
                weight: source.weight,
                depth_scale: source.depth_scale,
                depth_offset: source.depth_offset,
                multiplier: source.multiplier,
                sin_rotation: source.sin_rotation,
                cos_rotation: source.cos_rotation,
                pivot_shift: source.pivot_shift,
                pose_yaw: source.pose_yaw,
            })
            .collect(),
        weights: weights.cloned(),
        target_to_root,
        root_to_target,
        max_influences,
        radius_scale,
        radius_floor,
    }
}

fn compare_gpu(
    model: &Value,
    posed: &Value,
    root_id: u32,
    parameter: Option<u32>,
    keypoint: Option<[usize; 2]>,
    preview: &BTreeMap<u32, Vec<[f64; 2]>>,
    packets: &[(u32, crate::gpu_deform::Packet)],
    cpu: &[(u32, Vec<[f64; 2]>)],
) -> Result<f64, String> {
    let mut raw = Vec::new();
    for (target_id, packet) in packets {
        raw.push((*target_id, crate::gpu_deform::execute(packet)?));
    }
    let gpu = compensate_ancestor_grids(
        model, posed, root_id, parameter, keypoint, preview, raw,
    )?;
    max_abs_offset(cpu, &gpu)
}

fn max_abs_offset(
    cpu: &[(u32, Vec<[f64; 2]>)],
    gpu: &[(u32, Vec<[f64; 2]>)],
) -> Result<f64, String> {
    if cpu.len() != gpu.len() {
        return Err("gpu target count does not match".into());
    }
    let mut max_abs = 0.0f64;
    for ((cpu_id, cpu_offsets), (gpu_id, gpu_offsets)) in cpu.iter().zip(gpu.iter()) {
        if cpu_id != gpu_id || cpu_offsets.len() != gpu_offsets.len() {
            return Err("gpu offset layout does not match".into());
        }
        for (cpu_offset, gpu_offset) in cpu_offsets.iter().zip(gpu_offsets.iter()) {
            for (cpu_value, gpu_value) in cpu_offset.iter().zip(gpu_offset.iter()) {
                if !gpu_value.is_finite() {
                    return Err("non-finite gpu buffer".into());
                }
                max_abs = max_abs.max((cpu_value - gpu_value).abs());
            }
        }
    }
    if max_abs > ABSOLUTE_TOLERANCE {
        return Err(format!("gpu deform differs by {max_abs}"));
    }
    Ok(max_abs)
}

pub(crate) fn gaussian_vertex_weights(
    model: &Value,
    posed: &Value,
    root_id: u32,
    target_id: u32,
) -> Result<Vec<(u32, Vec<f64>)>, String> {
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let binding = binding_for(root, target_id).ok_or("Depth rig binding is required")?;
    let target = nodes::find_node(model, target_id)
        .ok_or_else(|| format!("node {target_id} was not found"))?;
    if !is_binding_target(target) {
        return Err("target is not deformable".into());
    }
    let vertices = target_vertices(target)?;
    let bones = build_runtime(model, root_id, None, None)?;
    let sources = source_records(binding, &bones)?;
    if sources.is_empty() {
        return Err("No valid depth bone sources".into());
    }
    let ids = binding
        .get("sourceBoneUuids")
        .and_then(Value::as_array)
        .ok_or("sourceBoneUuids must be an array")?;
    if ids.len() != sources.len() {
        return Err("vertex weight bone is not in this rig".into());
    }
    let rule = read_rule(binding.get("influenceRule"))?;
    let world_scale = depth_world_scale(model, posed, root_id)?;
    let raw_depths = snapshot_depths(target, vertices.len())?;
    let target_to_root = target_to_root(model, posed, root_id, target_id)?;
    let scaled: Vec<f64> = raw_depths.iter().map(|depth| depth * world_scale).collect();
    let mut prepared = Vec::new();
    for source in &sources {
        let bone = &bones[source.bone_index];
        let shift = rotation_pivot_shift(
            &vertices,
            &scaled,
            target_to_root,
            bone.rest_head,
            source,
            world_scale,
        )?;
        prepared.push(PreparedSource {
            bone_index: source.bone_index,
            terminal: source.terminal,
            lock_to_root: bone.lock_to_root,
            linear: rule.falloff == "linear",
            weight: source.weight,
            depth_scale: source.depth_scale,
            depth_offset: source.depth_offset * world_scale,
            multiplier: source.multiplier,
            sin_rotation: source.sin_rotation,
            cos_rotation: source.cos_rotation,
            pivot_shift: shift,
            pose_yaw: bone.pose_yaw,
        });
    }
    let radius_floor = rule
        .minimum_radius
        .max(target_bounds_size(&vertices) * 0.18);
    let mut rows = vec![vec![0.0; vertices.len()]; sources.len()];
    for (vertex_index, vertex) in vertices.iter().enumerate() {
        let z = scaled.get(vertex_index).copied().unwrap_or(0.0);
        let weights = normalized_gaussian_weights(
            *vertex,
            z,
            &prepared,
            &bones,
            rule.max_influences as usize,
            rule.radius_scale,
            radius_floor,
            target_to_root,
        )?;
        for (source_index, weight) in weights.into_iter().enumerate() {
            rows[source_index][vertex_index] = weight;
        }
    }
    let mut paired = Vec::with_capacity(sources.len());
    for (source, row) in sources.iter().zip(rows) {
        paired.push((bones[source.bone_index].id, row));
    }
    Ok(paired)
}

fn normalized_gaussian_weights(
    vertex: [f64; 2],
    z: f64,
    sources: &[PreparedSource],
    bones: &[BoneRuntime],
    max_influences: usize,
    radius_scale: f64,
    radius_floor: f64,
    target_to_root: Mat4,
) -> Result<Vec<f64>, String> {
    let limit = max_influences.min(MAX_INFLUENCES);
    let mut scores = [0.0; MAX_INFLUENCES];
    let mut distances = [0.0; MAX_INFLUENCES];
    let mut source_indices = [0usize; MAX_INFLUENCES];
    let mut rests = [[0.0; 3]; MAX_INFLUENCES];
    let mut count = 0usize;
    let mut locked: Option<(usize, f64, f64)> = None;
    for (source_index, source) in sources.iter().enumerate() {
        if source.bone_index >= bones.len() {
            continue;
        }
        let mut score = source.weight * source.multiplier;
        if !(score > 0.0) {
            continue;
        }
        let source_depth = z * source.depth_scale + source.depth_offset;
        let source_rest = transform_point(target_to_root, [vertex[0], vertex[1], source_depth])?;
        let bone = &bones[source.bone_index];
        let rest_length = bone.rest_length.max(1e-4);
        let mut distance_sq = 0.0;
        let mut terminal_distance_sq = f64::MAX;
        let mut terminal_projection = 0.0;
        let radius = (rest_length * 0.85 * radius_scale).max(radius_floor);
        let radius_sq = radius * radius;
        if sources.len() > 1 {
            distance_sq = distance_sq_point_segment(source_rest, bone.rest_head, bone.rest_tail);
            if source.linear {
                let distance = distance_sq.sqrt();
                score *= (1.0 - distance / radius).max(0.0);
            } else {
                score *= (-distance_sq / radius_sq).exp();
            }
            if source.terminal {
                if let Some(parent) = bone.parent_index {
                    if parent < bones.len() {
                        terminal_projection = point_segment_projection(
                            source_rest,
                            bones[parent].rest_head,
                            bone.rest_head,
                        );
                        terminal_distance_sq = distance_sq_point_segment(
                            source_rest,
                            bones[parent].rest_head,
                            bone.rest_head,
                        );
                    }
                }
            }
        }
        if !(score > 0.0) {
            continue;
        }
        if sources.len() > 1
            && source.terminal
            && source.lock_to_root
            && terminal_projection > 1.0
            && terminal_distance_sq <= radius_sq
        {
            let replace = match locked {
                None => true,
                Some((_, locked_score, locked_distance)) => {
                    score > locked_score
                        || (score == locked_score && terminal_distance_sq < locked_distance)
                }
            };
            if replace {
                locked = Some((source_index, score, terminal_distance_sq));
            }
            continue;
        }
        insert_influence(
            &mut scores,
            &mut distances,
            &mut source_indices,
            &mut rests,
            &mut count,
            limit,
            score,
            distance_sq,
            source_index,
            source_rest,
        );
    }
    if let Some((source_index, _, _)) = locked {
        count = 1;
        scores[0] = 1.0;
        source_indices[0] = source_index;
    }
    let mut total = 0.0;
    for score in scores.iter().take(count) {
        total += *score;
    }
    if count == 0 || !(total > 1e-8) {
        return Err("vertex weights are all zero".into());
    }
    let mut weights = vec![0.0; sources.len()];
    for index in 0..count {
        weights[source_indices[index]] = scores[index] / total;
    }
    Ok(weights)
}

fn persisted_weight_rows(
    binding: &Value,
    sources: &[SourceRecord],
    bones: &[BoneRuntime],
    vertex_count: usize,
) -> Result<Option<Vec<Vec<f64>>>, String> {
    let Some(raw) = binding.get("vertexWeights") else {
        return Ok(None);
    };
    let rows = raw.as_array().ok_or("vertexWeights must be an array")?;
    let ids = binding
        .get("sourceBoneUuids")
        .and_then(Value::as_array)
        .ok_or("sourceBoneUuids must be an array")?;
    if rows.len() != ids.len() || sources.len() != ids.len() {
        return Err("vertexWeights length must match the source list".into());
    }
    let mut aligned = Vec::with_capacity(sources.len());
    for source in sources {
        let bone_id = bones[source.bone_index].id;
        let Some(index) = ids
            .iter()
            .position(|id| id.as_u64() == Some(u64::from(bone_id)))
        else {
            return Err(format!("bone {bone_id} is not a source of this binding"));
        };
        let row = rows[index]
            .as_array()
            .ok_or("vertex weight row must be an array")?;
        if row.len() != vertex_count {
            return Err("vertex weight length must match the vertex count".into());
        }
        let mut values = Vec::with_capacity(row.len());
        for value in row {
            let weight = value
                .as_f64()
                .filter(|number| number.is_finite() && *number >= 0.0)
                .ok_or("vertex weight must be finite and >= 0")?;
            values.push(weight);
        }
        aligned.push(values);
    }
    for vertex in 0..vertex_count {
        let sum: f64 = aligned.iter().map(|row| row[vertex]).sum();
        if aligned.iter().all(|row| row[vertex] == 0.0) {
            return Err("vertex weights are all zero".into());
        }
        if (sum - 1.0).abs() > 1e-4 {
            return Err("vertex weights must sum to 1".into());
        }
    }
    Ok(Some(aligned))
}

struct SourceRecord {
    bone_index: usize,
    terminal: bool,
    weight: f64,
    depth_scale: f64,
    depth_offset: f64,
    rotation: f64,
    sin_rotation: f64,
    cos_rotation: f64,
    multiplier: f64,
}

struct PreparedSource {
    bone_index: usize,
    terminal: bool,
    lock_to_root: bool,
    linear: bool,
    weight: f64,
    depth_scale: f64,
    depth_offset: f64,
    multiplier: f64,
    sin_rotation: f64,
    cos_rotation: f64,
    pivot_shift: f64,
    pose_yaw: f64,
}

struct Rule {
    max_influences: u32,
    radius_scale: f64,
    minimum_radius: f64,
    falloff: String,
    multipliers: BTreeMap<u32, f64>,
}

fn source_records(binding: &Value, bones: &[BoneRuntime]) -> Result<Vec<SourceRecord>, String> {
    let ids = binding
        .get("sourceBoneUuids")
        .and_then(Value::as_array)
        .ok_or("sourceBoneUuids must be an array")?;
    let settings = binding.get("sourceSettings").and_then(Value::as_array);
    let rule = read_rule(binding.get("influenceRule"))?;
    let mut present = Vec::new();
    for id in ids {
        let Some(id) = id.as_u64().and_then(valid_id) else {
            continue;
        };
        if let Some(index) = bones.iter().position(|bone| bone.id == id) {
            present.push((id, index));
        }
    }
    let mut records = Vec::new();
    for (id, index) in &present {
        let setting = settings.and_then(|entries| {
            entries.iter().find(|entry| {
                entry.get("bone").and_then(Value::as_u64).and_then(valid_id) == Some(*id)
            })
        });
        let weight = setting
            .and_then(|value| value.get("weight"))
            .map(number_or)
            .unwrap_or(Ok(1.0))?;
        let depth_offset = setting
            .and_then(|value| value.get("depthOffset"))
            .map(number_or)
            .unwrap_or(Ok(0.0))?;
        let mut depth_scale = setting
            .and_then(|value| value.get("depthScale"))
            .map(number_or)
            .unwrap_or(Ok(1.0))?;
        if depth_scale < 0.01 {
            depth_scale = 0.01;
        }
        let rotation = setting
            .and_then(|value| value.get("rotation"))
            .map(|value| value.as_f64().unwrap_or(f64::NAN))
            .unwrap_or(0.0);
        let rotation = f64::from(normalize_rotation(rotation as f32));
        let weight = weight.max(0.0);
        let (sin_rotation, cos_rotation) = rotation.sin_cos();
        let terminal = !present.iter().any(|(other, other_index)| {
            other != id && bones[*other_index].parent_index == Some(*index)
        });
        let multiplier = rule.multipliers.get(id).copied().unwrap_or(1.0);
        records.push(SourceRecord {
            bone_index: *index,
            terminal,
            weight,
            depth_scale,
            depth_offset,
            rotation,
            sin_rotation,
            cos_rotation,
            multiplier,
        });
    }
    Ok(records)
}

fn read_rule(value: Option<&Value>) -> Result<Rule, String> {
    let mut rule = Rule {
        max_influences: 4,
        radius_scale: 1.0,
        minimum_radius: 1.0,
        falloff: "gaussian".into(),
        multipliers: BTreeMap::new(),
    };
    let Some(value) = value else {
        return Ok(rule);
    };
    if let Some(max) = value.get("maxInfluences") {
        let number = max
            .as_u64()
            .ok_or("maxInfluences must be an unsigned integer")?;
        let number = u32::try_from(number)
            .map_err(|_| "maxInfluences is outside the u32 range".to_string())?;
        rule.max_influences = if number == 0 { 1 } else { number };
    }
    if let Some(scale) = value.get("radiusScale") {
        rule.radius_scale = number_or(scale)?;
    }
    if let Some(radius) = value.get("minimumRadius") {
        rule.minimum_radius = number_or(radius)?;
    }
    if let Some(falloff) = value.get("falloff") {
        rule.falloff = falloff
            .as_str()
            .ok_or("falloff must be a string")?
            .to_owned();
    }
    if let Some(raw) = value
        .get("multipliersByBoneUuid")
        .and_then(Value::as_object)
    {
        for (key, multiplier) in raw {
            let id = key
                .parse::<u64>()
                .ok()
                .and_then(valid_id)
                .ok_or_else(|| format!("multipliersByBoneUuid.{key} must be a node UUID"))?;
            rule.multipliers.insert(id, number_or(multiplier)?);
        }
    }
    Ok(rule)
}

fn deform_vertex(
    vertex: [f64; 2],
    z: f64,
    sources: &[PreparedSource],
    bones: &[BoneRuntime],
    max_influences: usize,
    radius_scale: f64,
    radius_floor: f64,
    target_to_root: Mat4,
    root_to_target: Mat4,
) -> Result<[f64; 2], String> {
    let limit = max_influences.min(MAX_INFLUENCES);
    let mut scores = [0.0; MAX_INFLUENCES];
    let mut distances = [0.0; MAX_INFLUENCES];
    let mut source_indices = [0usize; MAX_INFLUENCES];
    let mut rests = [[0.0; 3]; MAX_INFLUENCES];
    let mut count = 0usize;
    let mut locked: Option<(usize, f64, f64, [f64; 3])> = None;
    for (source_index, source) in sources.iter().enumerate() {
        if source.bone_index >= bones.len() {
            continue;
        }
        let mut score = source.weight * source.multiplier;
        if !(score > 0.0) {
            continue;
        }
        let source_depth = z * source.depth_scale + source.depth_offset;
        let source_rest = transform_point(target_to_root, [vertex[0], vertex[1], source_depth])?;
        let bone = &bones[source.bone_index];
        let rest_length = bone.rest_length.max(1e-4);
        let mut distance_sq = 0.0;
        let mut terminal_distance_sq = f64::MAX;
        let mut terminal_projection = 0.0;
        let radius = (rest_length * 0.85 * radius_scale).max(radius_floor);
        let radius_sq = radius * radius;
        if sources.len() > 1 {
            distance_sq = distance_sq_point_segment(source_rest, bone.rest_head, bone.rest_tail);
            if source.linear {
                let distance = distance_sq.sqrt();
                score *= (1.0 - distance / radius).max(0.0);
            } else {
                score *= (-distance_sq / radius_sq).exp();
            }
            if source.terminal {
                if let Some(parent) = bone.parent_index {
                    if parent < bones.len() {
                        terminal_projection = point_segment_projection(
                            source_rest,
                            bones[parent].rest_head,
                            bone.rest_head,
                        );
                        terminal_distance_sq = distance_sq_point_segment(
                            source_rest,
                            bones[parent].rest_head,
                            bone.rest_head,
                        );
                    }
                }
            }
        }
        if !(score > 0.0) {
            continue;
        }
        if sources.len() > 1
            && source.terminal
            && source.lock_to_root
            && terminal_projection > 1.0
            && terminal_distance_sq <= radius_sq
        {
            let replace = match locked {
                None => true,
                Some((_, locked_score, locked_distance, _)) => {
                    score > locked_score
                        || (score == locked_score && terminal_distance_sq < locked_distance)
                }
            };
            if replace {
                locked = Some((source_index, score, terminal_distance_sq, source_rest));
            }
            continue;
        }
        insert_influence(
            &mut scores,
            &mut distances,
            &mut source_indices,
            &mut rests,
            &mut count,
            limit,
            score,
            distance_sq,
            source_index,
            source_rest,
        );
    }
    if let Some((source_index, _, _, rest)) = locked {
        count = 1;
        scores[0] = 1.0;
        source_indices[0] = source_index;
        rests[0] = rest;
    }
    if count == 0 {
        return Ok([0.0, 0.0]);
    }
    let mut total = 0.0;
    for score in scores.iter().take(count) {
        total += *score;
    }
    if !(total > 1e-8) {
        total = 1.0;
        count = 1;
        scores[0] = 1.0;
    }
    let mut rest = [0.0; 3];
    let mut deformed = [0.0; 3];
    for index in 0..count {
        let weight = scores[index] / total;
        rest = add(rest, scale3(rests[index], weight));
        let source = &sources[source_indices[index]];
        let full = bones[source.bone_index].skin;
        let point = if source.pose_yaw == 0.0
            || source.pivot_shift == 0.0
            || (source.sin_rotation == 0.0 && source.cos_rotation == 1.0)
        {
            transform_point(full, rests[index])?
        } else {
            let rotation_rest = add(rests[index], [source.pivot_shift, 0.0, 0.0]);
            let no_yaw = bones[source.bone_index].no_yaw_skin;
            let base = transform_point(no_yaw, rests[index])?;
            let with_yaw = transform_point(full, rotation_rest)?;
            let without_yaw = transform_point(no_yaw, rotation_rest)?;
            add(base, sub(with_yaw, without_yaw))
        };
        deformed = add(deformed, scale3(point, weight));
    }
    let rest_local = transform_point(root_to_target, rest)?;
    let local = transform_point(root_to_target, deformed)?;
    let offset = [local[0] - rest_local[0], local[1] - rest_local[1]];
    if !offset[0].is_finite() || !offset[1].is_finite() {
        return Err("non-finite matrix".into());
    }
    Ok(offset)
}

fn deform_vertex_weighted(
    vertex: [f64; 2],
    z: f64,
    sources: &[PreparedSource],
    bones: &[BoneRuntime],
    weights: &[Vec<f64>],
    vertex_index: usize,
    target_to_root: Mat4,
    root_to_target: Mat4,
) -> Result<[f64; 2], String> {
    let mut rest = [0.0; 3];
    let mut deformed = [0.0; 3];
    for (source_index, source) in sources.iter().enumerate() {
        let weight = weights[source_index][vertex_index];
        if weight == 0.0 || source.bone_index >= bones.len() {
            continue;
        }
        let source_depth = z * source.depth_scale + source.depth_offset;
        let source_rest = transform_point(target_to_root, [vertex[0], vertex[1], source_depth])?;
        let point = if source.pose_yaw == 0.0
            || source.pivot_shift == 0.0
            || (source.sin_rotation == 0.0 && source.cos_rotation == 1.0)
        {
            transform_point(bones[source.bone_index].skin, source_rest)?
        } else {
            let rotation_rest = add(source_rest, [source.pivot_shift, 0.0, 0.0]);
            let full = bones[source.bone_index].skin;
            let no_yaw = bones[source.bone_index].no_yaw_skin;
            let base = transform_point(no_yaw, source_rest)?;
            let with_yaw = transform_point(full, rotation_rest)?;
            let without_yaw = transform_point(no_yaw, rotation_rest)?;
            add(base, sub(with_yaw, without_yaw))
        };
        rest = add(rest, scale3(source_rest, weight));
        deformed = add(deformed, scale3(point, weight));
    }
    let rest_local = transform_point(root_to_target, rest)?;
    let local = transform_point(root_to_target, deformed)?;
    let offset = [local[0] - rest_local[0], local[1] - rest_local[1]];
    if !offset[0].is_finite() || !offset[1].is_finite() {
        return Err("non-finite matrix".into());
    }
    Ok(offset)
}

fn insert_influence(
    scores: &mut [f64],
    distances: &mut [f64],
    source_indices: &mut [usize],
    rests: &mut [[f64; 3]],
    count: &mut usize,
    limit: usize,
    score: f64,
    distance_sq: f64,
    source_index: usize,
    rest: [f64; 3],
) {
    if limit == 0 {
        return;
    }
    let mut pos = *count;
    if *count < limit {
        *count += 1;
    } else {
        pos = limit - 1;
        if score < scores[pos] || (score == scores[pos] && distance_sq >= distances[pos]) {
            return;
        }
    }
    while pos > 0 {
        let prev = pos - 1;
        if score < scores[prev] || (score == scores[prev] && distance_sq >= distances[prev]) {
            break;
        }
        scores[pos] = scores[prev];
        distances[pos] = distances[prev];
        source_indices[pos] = source_indices[prev];
        rests[pos] = rests[prev];
        pos = prev;
    }
    scores[pos] = score;
    distances[pos] = distance_sq;
    source_indices[pos] = source_index;
    rests[pos] = rest;
}

fn rotation_pivot_shift(
    vertices: &[[f64; 2]],
    scaled_depths: &[f64],
    target_to_root: Mat4,
    rest_head: [f64; 3],
    source: &SourceRecord,
    world_scale: f64,
) -> Result<f64, String> {
    let mut plane_depth = 0.0;
    let mut best = f64::INFINITY;
    let mut found = false;
    for (index, vertex) in vertices.iter().enumerate() {
        if index >= scaled_depths.len() {
            break;
        }
        let adjusted =
            scaled_depths[index] * source.depth_scale + source.depth_offset * world_scale;
        if !adjusted.is_finite() {
            continue;
        }
        let sample = transform_point(target_to_root, [vertex[0], vertex[1], adjusted])?;
        let dx = sample[0] - rest_head[0];
        let dy = sample[1] - rest_head[1];
        let distance_sq = dx * dx + dy * dy;
        if !found || distance_sq < best {
            found = true;
            best = distance_sq;
            plane_depth = sample[2];
        }
    }
    if !found {
        let sample = transform_point(
            target_to_root,
            [0.0, 0.0, source.depth_offset * world_scale],
        )?;
        plane_depth = if sample[2].is_finite() {
            sample[2]
        } else {
            0.0
        };
    }
    let slope = tangent_slope(source.rotation);
    let shift = (plane_depth - rest_head[2]) * slope;
    if !shift.is_finite() {
        return Err("non-finite matrix".into());
    }
    Ok(shift)
}

fn tangent_slope(rotation: f64) -> f64 {
    let (sin, mut cos) = rotation.sin_cos();
    if cos.abs() < 1e-4 {
        cos = if cos < 0.0 { -1e-4 } else { 1e-4 };
    }
    sin / cos
}

fn snapshot_depths(node: &Value, count: usize) -> Result<Vec<f64>, String> {
    let mut depths = vec![0.0; count];
    let Some(stored) = node.get("depths").and_then(Value::as_array) else {
        return Ok(depths);
    };
    for (index, value) in stored.iter().take(count).enumerate() {
        depths[index] = value.as_f64().filter(|n| n.is_finite()).unwrap_or(0.0);
    }
    Ok(depths)
}

/// A node a rig binding may target: its vertices are moved by the bones, each at its own depth. A Part is bound by
/// its own mesh vertices. As for a grid, the rig writes the target's `"deform"` row for every parameter that drives
/// a bone, and while a parameter drives the rig the target takes no manual deform key (`deformers::set_deform`).
pub(crate) fn is_binding_target(node: &Value) -> bool {
    matches!(node.get("type").and_then(Value::as_str), Some("GridDeformer" | "PathDeformer" | "Part"))
}

pub(crate) fn target_vertices(node: &Value) -> Result<Vec<[f64; 2]>, String> {
    let pairs = |raw: Option<&Value>| -> Result<Vec<[f64; 2]>, String> {
        let Some(raw) = raw.and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        if raw.len() % 2 != 0 {
            return Err("offset length mismatch".into());
        }
        let number = |value: &Value| value.as_f64().filter(|n| n.is_finite()).ok_or("offset length mismatch");
        raw.chunks(2).map(|pair| Ok([number(&pair[0])?, number(&pair[1])?])).collect()
    };
    match node.get("type").and_then(Value::as_str) {
        Some("GridDeformer") => {
            let xs = axis(node, "grid_axis_x")?;
            let ys = axis(node, "grid_axis_y")?;
            if xs.len() < 2 || ys.len() < 2 {
                return Err("target is not deformable".into());
            }
            let mut points = Vec::with_capacity(xs.len() * ys.len());
            for y in &ys {
                for x in &xs {
                    points.push([*x, *y]);
                }
            }
            Ok(points)
        }
        Some("PathDeformer") => pairs(node.get("vertices")),
        Some("Part") => {
            let mesh = node.get("mesh");
            // The renderer draws a Part at `verts - origin`, and those are the points a bone moves.
            let origin = crate::deformers::mesh_origin(mesh)?;
            let stored = pairs(mesh.and_then(|mesh| mesh.get("verts")))?;
            Ok(stored.into_iter().map(|[x, y]| [x - origin[0], y - origin[1]]).collect())
        }
        _ => Err("target is not deformable".into()),
    }
}

fn axis(node: &Value, key: &str) -> Result<Vec<f64>, String> {
    let Some(raw) = node.get(key).and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    raw.iter()
        .map(|value| {
            value
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or_else(|| "target is not deformable".to_string())
        })
        .collect()
}

fn target_bounds_size(vertices: &[[f64; 2]]) -> f64 {
    let Some(first) = vertices.first() else {
        return 1.0;
    };
    let mut min = *first;
    let mut max = *first;
    for vertex in vertices.iter().skip(1) {
        min[0] = min[0].min(vertex[0]);
        min[1] = min[1].min(vertex[1]);
        max[0] = max[0].max(vertex[0]);
        max[1] = max[1].max(vertex[1]);
    }
    1.0_f64.max((max[0] - min[0]).max(max[1] - min[1]))
}

fn skip_bad_pivot<T>(result: Result<T, String>) -> Result<Option<T>, String> {
    match result {
        Ok(value) => Ok(Some(value)),
        Err(error) if error == "non-finite matrix" => Err(error),
        Err(_) => Ok(None),
    }
}

pub(crate) fn depth_world_scale(model: &Value, posed: &Value, root_id: u32) -> Result<f64, String> {
    depth_world_scale_inner(model, posed, root_id, true)
}

fn depth_world_scale_inner(
    model: &Value,
    posed: &Value,
    root_id: u32,
    strict: bool,
) -> Result<f64, String> {
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let Some(bindings) = root.get("bindings").and_then(Value::as_array) else {
        return Ok(1.0);
    };
    let mut min = [0.0; 2];
    let mut max = [0.0; 2];
    let mut has = false;
    for binding in bindings {
        let Some(id) = binding
            .get("target")
            .and_then(Value::as_u64)
            .and_then(valid_id)
        else {
            continue;
        };
        let Some(target) = nodes::find_node(model, id) else {
            continue;
        };
        if !is_binding_target(target) {
            continue;
        }
        let vertices = match target_vertices(target) {
            Ok(vertices) => vertices,
            Err(error) if strict || error == "non-finite matrix" => return Err(error),
            Err(_) => continue,
        };
        let matrix = match target_to_root(model, posed, root_id, id) {
            Ok(matrix) => matrix,
            Err(error) if strict || error == "non-finite matrix" => return Err(error),
            Err(_) => continue,
        };
        for vertex in vertices {
            let point = transform_point(matrix, [vertex[0], vertex[1], 0.0])?;
            if !point[0].is_finite() || !point[1].is_finite() {
                continue;
            }
            if !has {
                min = [point[0], point[1]];
                max = min;
                has = true;
            } else {
                min[0] = min[0].min(point[0]);
                min[1] = min[1].min(point[1]);
                max[0] = max[0].max(point[0]);
                max[1] = max[1].max(point[1]);
            }
        }
    }
    if !has {
        return Ok(1.0);
    }
    let size = (max[0] - min[0]).max(max[1] - min[1]);
    let scaled = (size as f32) * (0.42f32 / 2.9f32);
    if !scaled.is_finite() {
        return Err("non-finite matrix".into());
    }
    Ok(f64::from(scaled.max(1.0)))
}

fn target_to_root(
    model: &Value,
    posed: &Value,
    root_id: u32,
    target_id: u32,
) -> Result<Mat4, String> {
    let root = world_matrix(posed, root_id)?;
    let target = world_matrix(posed, target_id)?;
    let _ = model;
    Ok(mul_mat(invert(root)?, target))
}

pub(crate) fn world_matrix(model: &Value, id: u32) -> Result<Mat4, String> {
    let root = model.get("nodes").ok_or("node was not found")?;
    world_matrix_with_root(model, id, root)
}

pub(crate) fn world_matrix_with_root(
    model: &Value,
    id: u32,
    original_root: &Value,
) -> Result<Mat4, String> {
    let mut chain = Vec::new();
    let mut current = Some(id);
    while let Some(node_id) = current {
        chain.push(node_id);
        current = parent_of(model, node_id)?;
    }
    chain.reverse();
    let root_id = chain.first().copied().ok_or("node was not found")?;
    let mut worlds = BTreeMap::<u32, Mat4>::new();
    for node_id in chain {
        let node = nodes::find_node(model, node_id)
            .ok_or_else(|| format!("node {node_id} was not found"))?;
        let local = local_matrix(node)?;
        let world = if flag(node, "lockToRoot") && node_id != root_id {
            mul_mat(local_matrix(original_root)?, local)
        } else if let Some(parent) = parent_of(model, node_id)? {
            mul_mat(worlds.get(&parent).copied().unwrap_or_else(identity), local)
        } else {
            local
        };
        if !matrix_finite(world) {
            return Err("non-finite matrix".into());
        }
        worlds.insert(node_id, world);
    }
    worlds
        .get(&id)
        .copied()
        .ok_or_else(|| "non-finite matrix".into())
}

/// `T * R * S` for the headless raster, including local Z. The draw keeps
/// only the xy of `matrix * vec4(x, y, 0, 1)`, so Z alone does not move a
/// sprite, but an ancestor X/Y rotation can swing that Z into the screen.
pub(crate) fn projection_matrix(
    translation: [f64; 3],
    rotation: [f64; 3],
    scale: [f64; 2],
) -> Result<Mat4, String> {
    let matrix = mul_mat(
        translation_matrix(translation),
        mul_mat(
            quat_matrix(euler_rotation(rotation[0], rotation[1], rotation[2])),
            scale_matrix([scale[0], scale[1], 1.0]),
        ),
    );
    if !matrix_finite(matrix) {
        return Err("non-finite matrix".into());
    }
    Ok(matrix)
}

fn local_matrix(node: &Value) -> Result<Mat4, String> {
    let translation = transform_vec(node, "trans", [0.0, 0.0, 0.0]);
    let rotation = transform_vec(node, "rot", [0.0, 0.0, 0.0]);
    let scale = transform_vec(node, "scale", [1.0, 1.0, 1.0]);
    let matrix = mul_mat(
        translation_matrix(translation),
        mul_mat(
            quat_matrix(euler_rotation(rotation[0], rotation[1], rotation[2])),
            scale_matrix([scale[0], scale[1], 1.0]),
        ),
    );
    if !matrix_finite(matrix) {
        return Err("non-finite matrix".into());
    }
    Ok(matrix)
}

fn effective_rest(
    model: &Value,
    root_id: u32,
    bone_id: u32,
) -> Result<([f64; 3], [f64; 3]), String> {
    let head = rest_position(model, root_id, bone_id)?;
    let bone =
        nodes::find_node(model, bone_id).ok_or_else(|| format!("node {bone_id} was not found"))?;
    let stored_head = vec3_field(bone, "restHead");
    let stored_tail = vec3_field(bone, "restTail");
    let direction = if let Some(child) = first_child_bone(bone) {
        let child_id = child
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or("node is missing uuid")?;
        planar_direction(
            sub(rest_position(model, root_id, child_id)?, head),
            sub(stored_tail, stored_head),
        )
    } else if let Some(parent) = parent_of(model, bone_id)? {
        let parent_node = nodes::find_node(model, parent)
            .ok_or_else(|| format!("node {parent} was not found"))?;
        if parent_node.get("type").and_then(Value::as_str) == Some("DepthBone") {
            planar_direction(
                sub(head, rest_position(model, root_id, parent)?),
                sub(stored_tail, stored_head),
            )
        } else {
            planar_direction([0.0; 3], sub(stored_tail, stored_head))
        }
    } else {
        planar_direction([0.0; 3], sub(stored_tail, stored_head))
    };
    Ok((head, add(head, direction)))
}

fn planar_direction(primary: [f64; 3], fallback: [f64; 3]) -> [f64; 3] {
    let primary = [primary[0], primary[1], 0.0];
    if distance([0.0; 3], primary) > 1e-4 {
        return primary;
    }
    let fallback = [fallback[0], fallback[1], 0.0];
    if distance([0.0; 3], fallback) > 1e-4 {
        return fallback;
    }
    [0.0, 100.0, 0.0]
}

fn rest_position(model: &Value, root_id: u32, bone_id: u32) -> Result<[f64; 3], String> {
    let mut result = [0.0; 3];
    let mut current = Some(bone_id);
    while let Some(id) = current {
        if id == root_id {
            break;
        }
        let node = nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
        result = add(result, transform_vec(node, "trans", [0.0, 0.0, 0.0]));
        if flag(node, "lockToRoot") {
            break;
        }
        current = parent_of(model, id)?;
    }
    Ok(result)
}

fn first_child_bone(node: &Value) -> Option<&Value> {
    node.get("children")
        .and_then(Value::as_array)
        .and_then(|children| {
            children
                .iter()
                .find(|child| child.get("type").and_then(Value::as_str) == Some("DepthBone"))
        })
}

pub(crate) fn bone_order(model: &Value, root_id: u32) -> Result<Vec<u32>, String> {
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let mut ids = Vec::new();
    if let Some(children) = root.get("children").and_then(Value::as_array) {
        for child in children {
            collect_bones(child, &mut ids)?;
        }
    }
    Ok(ids)
}

fn collect_bones(node: &Value, ids: &mut Vec<u32>) -> Result<(), String> {
    if node.get("type").and_then(Value::as_str) == Some("DepthBone") {
        let id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or("node is missing uuid")?;
        ids.push(id);
    }
    if let Some(children) = node.get("children").and_then(Value::as_array) {
        for child in children {
            collect_bones(child, ids)?;
        }
    }
    Ok(())
}

fn write_deform_key(
    model: &mut Value,
    parameter_id: u32,
    node_id: u32,
    keypoint: [usize; 2],
    offsets: &[[f64; 2]],
    replace_lengths: bool,
) -> Result<(), String> {
    let (x_axis, y_axis) = {
        let parameter = find_parameter(model, parameter_id)
            .ok_or_else(|| format!("parameter {parameter_id} was not found"))?;
        let (x_axis, y_axis, _) = crate::deformers::deform_key_axes(parameter)?;
        (x_axis, y_axis)
    };
    let parameter = find_parameter_mut(model, parameter_id)
        .ok_or_else(|| format!("parameter {parameter_id} was not found"))?;
    let repeat = crate::evaluation::parameter_repeats(parameter);
    let x_count = x_axis.len();
    let y_count = y_axis.len();
    if keypoint[0] >= x_count || keypoint[1] >= y_count || x_count == 0 || y_count == 0 {
        return Err("missing \"deform\" binding support".into());
    }
    let bindings = parameter
        .as_object_mut()
        .ok_or("parameter must be an object")?
        .entry("bindings")
        .or_insert_with(|| json!([]));
    let bindings = bindings.as_array_mut().ok_or("bindings must be an array")?;
    let index = bindings.iter().position(|binding| {
        binding
            .get("node")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            == Some(node_id)
            && binding.get("param_name").and_then(Value::as_str) == Some("deform")
    });
    let binding = if let Some(index) = index {
        &mut bindings[index]
    } else {
        bindings.push(json!({
            "node": node_id,
            "param_name": "deform",
            "values": empty_grid(x_count, y_count),
            "isSet": false_grid(x_count, y_count),
            "interpolate_mode": "Linear",
        }));
        bindings.last_mut().unwrap()
    };
    ensure_deform_grid(binding, x_count, y_count, offsets.len(), replace_lengths)?;
    crate::deformers::update_deform_key(
        binding,
        node_id,
        keypoint,
        offsets,
        &x_axis,
        &y_axis,
        replace_lengths,
        crate::deformers::UnsetFillOverflow::Defer,
        repeat,
    )
}

fn ensure_deform_grid(
    binding: &mut Value,
    x_count: usize,
    y_count: usize,
    vertex_count: usize,
    replace_lengths: bool,
) -> Result<(), String> {
    let object = binding.as_object_mut().ok_or("binding must be an object")?;
    if !object.contains_key("values") {
        object.insert("values".into(), Value::Array(empty_grid(x_count, y_count)));
    }
    if !object.contains_key("isSet") {
        object.insert("isSet".into(), Value::Array(false_grid(x_count, y_count)));
    }
    let values = object
        .get("values")
        .ok_or("missing \"deform\" binding support")?;
    let flags = object
        .get("isSet")
        .ok_or("missing \"deform\" binding support")?;
    deform_grid_shape(values, x_count, y_count, Some(vertex_count), replace_lengths)?;
    deform_grid_shape(flags, x_count, y_count, None, false)?;
    Ok(())
}

fn deform_grid_shape(
    grid: &Value,
    x_count: usize,
    y_count: usize,
    vertex_count: Option<usize>,
    replace_lengths: bool,
) -> Result<(), String> {
    let columns = grid
        .as_array()
        .ok_or("missing \"deform\" binding support")?;
    if columns.len() != x_count {
        return Err("missing \"deform\" binding support".into());
    }
    for column in columns {
        let column = column
            .as_array()
            .ok_or("missing \"deform\" binding support")?;
        if column.len() != y_count {
            return Err("missing \"deform\" binding support".into());
        }
        if vertex_count.is_none() {
            if column
                .iter()
                .any(|flag| !flag.is_boolean() && !flag.is_null())
            {
                return Err("missing \"deform\" binding support".into());
            }
            continue;
        }
        let vertex_count = vertex_count.unwrap();
        for cell in column {
            match cell {
                Value::Array(points) => {
                    if !replace_lengths && !points.is_empty() && points.len() != vertex_count {
                        return Err("offset length mismatch".into());
                    }
                    for point in points {
                        let pair = point.as_array().ok_or("offset length mismatch")?;
                        if pair.len() < 2
                            || pair
                                .iter()
                                .take(2)
                                .any(|value| value.as_f64().is_none_or(|n| !n.is_finite()))
                        {
                            return Err("offset length mismatch".into());
                        }
                    }
                }
                Value::Null => {}
                _ => return Err("missing \"deform\" binding support".into()),
            }
        }
    }
    Ok(())
}

fn empty_grid(x_count: usize, y_count: usize) -> Vec<Value> {
    (0..x_count)
        .map(|_| Value::Array(vec![Value::Array(Vec::new()); y_count]))
        .collect()
}

fn false_grid(x_count: usize, y_count: usize) -> Vec<Value> {
    (0..x_count)
        .map(|_| Value::Array(vec![json!(false); y_count]))
        .collect()
}

fn binding_for(root: &Value, target_id: u32) -> Option<&Value> {
    root.get("bindings")
        .and_then(Value::as_array)
        .and_then(|bindings| {
            bindings.iter().find(|binding| {
                binding
                    .get("target")
                    .and_then(Value::as_u64)
                    .and_then(valid_id)
                    == Some(target_id)
            })
        })
}

fn contains_node(node: &Value, id: u32) -> bool {
    node.get("children")
        .and_then(Value::as_array)
        .is_some_and(|children| {
            children.iter().any(|child| {
                child.get("uuid").and_then(Value::as_u64).and_then(valid_id) == Some(id)
                    || contains_node(child, id)
            })
        })
}

pub(crate) fn parent_of(model: &Value, id: u32) -> Result<Option<u32>, String> {
    fn walk(node: &Value, id: u32, parent: Option<u32>) -> Result<Option<Option<u32>>, String> {
        let node_id = node
            .get("uuid")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or("node.uuid must be an unsigned integer")?;
        if node_id == id {
            return Ok(Some(parent));
        }
        if let Some(children) = node.get("children").and_then(Value::as_array) {
            for child in children {
                if let Some(found) = walk(child, id, Some(node_id))? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
    let root = model.get("nodes").ok_or("model is missing nodes")?;
    walk(root, id, None)?.ok_or_else(|| format!("node {id} was not found"))
}

fn find_parameter<'a>(model: &'a Value, id: u32) -> Option<&'a Value> {
    fn find(list: &[Value], id: u32) -> Option<&Value> {
        for item in list {
            if item.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
                return Some(item);
            }
            if let Some(children) = item.get("children").and_then(Value::as_array) {
                if let Some(found) = find(children, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    model
        .get("param")
        .and_then(Value::as_array)
        .and_then(|list| find(list, id))
}

fn find_parameter_mut(model: &mut Value, id: u32) -> Option<&mut Value> {
    fn find(list: &mut [Value], id: u32) -> Option<&mut Value> {
        for item in list {
            if item.get("uuid").and_then(Value::as_u64) == Some(u64::from(id)) {
                return Some(item);
            }
            if let Some(children) = item.get_mut("children").and_then(Value::as_array_mut) {
                if let Some(found) = find(children, id) {
                    return Some(found);
                }
            }
        }
        None
    }
    model
        .get_mut("param")
        .and_then(Value::as_array_mut)
        .and_then(|list| find(list, id))
}

fn flag(node: &Value, key: &str) -> bool {
    node.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn number_field(node: &Value, key: &str, fallback: f64) -> f64 {
    node.get(key)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .unwrap_or(fallback)
}

fn vec3_field(node: &Value, key: &str) -> [f64; 3] {
    let Some(value) = node.get(key).and_then(Value::as_array) else {
        return [0.0; 3];
    };
    let mut output = [0.0; 3];
    for (index, component) in value.iter().take(3).enumerate() {
        output[index] = component.as_f64().unwrap_or(0.0);
    }
    output
}

pub(crate) fn transform_vec(node: &Value, key: &str, fallback: [f64; 3]) -> [f64; 3] {
    let Some(transform) = node.get("transform") else {
        return fallback;
    };
    let Some(value) = transform.get(key).and_then(Value::as_array) else {
        return fallback;
    };
    let mut output = fallback;
    for (index, component) in value.iter().take(3).enumerate() {
        if let Some(number) = component.as_f64().filter(|n| n.is_finite()) {
            output[index] = number;
        }
    }
    output
}

fn pair(value: Option<&Value>, fallback: [f64; 2], name: &str) -> Result<[f64; 2], String> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    let array = value
        .as_array()
        .ok_or_else(|| format!("{name} must contain two finite numbers"))?;
    if array.len() != 2 {
        return Err(format!("{name} must contain two finite numbers"));
    }
    Ok([
        array[0]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("{name} must contain finite numbers"))?,
        array[1]
            .as_f64()
            .filter(|n| n.is_finite())
            .ok_or_else(|| format!("{name} must contain finite numbers"))?,
    ])
}

fn number_or(value: &Value) -> Result<f64, String> {
    value
        .as_f64()
        .filter(|n| n.is_finite())
        .ok_or_else(|| "value must be finite".into())
}

fn normalize_rotation(rotation: f32) -> f32 {
    if !rotation.is_finite() {
        return 0.0;
    }
    let pi = std::f32::consts::PI;
    let mut normalized = (rotation + pi) % (pi * 2.0);
    if normalized < 0.0 {
        normalized += pi * 2.0;
    }
    normalized - pi
}

fn add(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn scale3(a: [f64; 3], scale: f64) -> [f64; 3] {
    [a[0] * scale, a[1] * scale, a[2] * scale]
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = sub(a, b);
    (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt()
}

fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize3(value: [f64; 3]) -> [f64; 3] {
    let length = distance(value, [0.0; 3]);
    if length == 0.0 {
        value
    } else {
        scale3(value, 1.0 / length)
    }
}

fn point_segment_projection(point: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let ab = sub(b, a);
    let denom = dot(ab, ab);
    if denom <= 1e-8 {
        return 0.0;
    }
    dot(sub(point, a), ab) / denom
}

fn distance_sq_point_segment(point: [f64; 3], a: [f64; 3], b: [f64; 3]) -> f64 {
    let t = point_segment_projection(point, a, b).clamp(0.0, 1.0);
    let q = add(a, scale3(sub(b, a), t));
    let d = sub(point, q);
    dot(d, d)
}

fn identity_quat() -> Quat {
    Quat {
        w: 1.0,
        x: 0.0,
        y: 0.0,
        z: 0.0,
    }
}

fn quat_from_unit(from: [f64; 3], to: [f64; 3]) -> Quat {
    let from = normalize3(from);
    let to = normalize3(to);
    let mut r = dot(from, to) + 1.0;
    let axis = if r < 1e-6 {
        r = 0.0;
        if from[0].abs() > from[2].abs() {
            [-from[1], from[0], 0.0]
        } else {
            [0.0, -from[2], from[1]]
        }
    } else {
        cross(from, to)
    };
    normalize_quat(Quat {
        w: r,
        x: axis[0],
        y: axis[1],
        z: axis[2],
    })
}

fn axis_rotation(alpha: f64, axis: [f64; 3]) -> Quat {
    if alpha == 0.0 {
        return identity_quat();
    }
    let half = alpha / 2.0;
    let sine = half.sin();
    Quat {
        w: half.cos(),
        x: axis[0] * sine,
        y: axis[1] * sine,
        z: axis[2] * sine,
    }
}

fn euler_rotation(roll: f64, pitch: f64, yaw: f64) -> Quat {
    let cr = (roll / 2.0).cos();
    let cp = (pitch / 2.0).cos();
    let cy = (yaw / 2.0).cos();
    let sr = (roll / 2.0).sin();
    let sp = (pitch / 2.0).sin();
    let sy = (yaw / 2.0).sin();
    Quat {
        w: cr * cp * cy + sr * sp * sy,
        x: sr * cp * cy - cr * sp * sy,
        y: cr * sp * cy + sr * cp * sy,
        z: cr * cp * sy - sr * sp * cy,
    }
}

fn depth_edit_rotation(pitch: f64, yaw: f64, roll: f64) -> Quat {
    euler_rotation(-pitch, -yaw, roll)
}

/// Rest orientation of a bone: the rotation taking +y to `tail - head`, then the bone roll about its own y.
fn rest_quat(head: [f64; 3], tail: [f64; 3], roll: f64) -> Quat {
    mul_quat(quat_from_unit([0.0, 1.0, 0.0], sub(tail, head)), axis_rotation(roll, [0.0, 1.0, 0.0]))
}

/// `rest_quat` as `[w, x, y, z]`, for `mmd_motion` (a playback slice reads it; only its tests do so far).
#[cfg(test)]
pub(crate) fn rest_quat_wxyz(head: [f64; 3], tail: [f64; 3], roll: f64) -> [f64; 4] {
    let quat = rest_quat(head, tail, roll);
    [quat.w, quat.x, quat.y, quat.z]
}

/// What `mmd_motion::apply` needs of one bone: the rest orientation `[w, x, y, z]` and rest head that
/// `build_runtime_limited` uses (effective rest, so the direction follows the first child bone), the DepthBone parent,
/// whether the bone follows that parent's pose, and the rotation and translation locks.
pub(crate) struct BoneRestInfo {
    pub rest: [f64; 4],
    pub head: [f64; 3],
    pub parent: Option<u32>,
    pub follows_parent: bool,
    pub lock_rotation: bool,
    pub lock_translation: bool,
}

pub(crate) fn bone_rest_info(model: &Value, root_id: u32, bone_id: u32) -> Result<BoneRestInfo, String> {
    let node = nodes::find_node(model, bone_id).ok_or_else(|| format!("node {bone_id} was not found"))?;
    let (head, tail) = effective_rest(model, root_id, bone_id)?;
    let rest = rest_quat(head, tail, number_field(node, "restRoll", 0.0));
    let parent = parent_of(model, bone_id)?.filter(|parent| {
        nodes::find_node(model, *parent).and_then(|node| node.get("type")).and_then(Value::as_str) == Some("DepthBone")
    });
    Ok(BoneRestInfo {
        rest: [rest.w, rest.x, rest.y, rest.z],
        head,
        parent,
        follows_parent: node.get("allowParentToTargets").and_then(Value::as_bool).unwrap_or(true)
            && !flag(node, "lockToRoot"),
        lock_rotation: flag(node, "lockRotation"),
        lock_translation: flag(node, "lockTranslation"),
    })
}

/// World rotation `[w, x, y, z]` of a bone with the given rest orientation and pose angles whose ancestors are
/// unposed: `world_prefix * pose_quat` as `build_runtime_limited` computes it, where the prefix is the rest
/// orientation. Test oracle for `mmd_motion`.
#[cfg(test)]
pub(crate) fn world_quat_wxyz(rest: [f64; 4], pitch: f64, yaw: f64, roll: f64) -> [f64; 4] {
    let rest = Quat { w: rest[0], x: rest[1], y: rest[2], z: rest[3] };
    let quat = mul_quat(rest, depth_edit_rotation(pitch, yaw, roll));
    [quat.w, quat.x, quat.y, quat.z]
}

/// World rotation `[w, x, y, z]` of every bone of the rig as the deform path poses it at a parameter keypoint
/// (`build_runtime_limited`, ancestors' poses included). Test oracle for `mmd_motion`.
#[cfg(test)]
pub(crate) fn keypoint_world_quats_wxyz(
    model: &Value,
    root_id: u32,
    parameter: u32,
    key: [usize; 2],
) -> Result<BTreeMap<u32, [f64; 4]>, String> {
    let bones = build_runtime_limited(model, root_id, Some(parameter), Some(key), false)?;
    Ok(bones.iter().map(|bone| (bone.id, [bone.world_quat.w, bone.world_quat.x, bone.world_quat.y, bone.world_quat.z])).collect())
}

/// `depth_edit_rotation` as `[w, x, y, z]`, for `mmd_motion`.
pub(crate) fn depth_edit_rotation_wxyz(pitch: f64, yaw: f64, roll: f64) -> [f64; 4] {
    let quat = depth_edit_rotation(pitch, yaw, roll);
    [quat.w, quat.x, quat.y, quat.z]
}

/// `euler_from_depth_edit` for a `[w, x, y, z]` quaternion, for `mmd_motion`.
pub(crate) fn euler_from_depth_edit_wxyz(quat: [f64; 4]) -> Result<[f64; 3], String> {
    euler_from_depth_edit(Quat { w: quat[0], x: quat[1], y: quat[2], z: quat[3] })
}

/// Inverse of `depth_edit_rotation`. The matrix is `Rz(roll) * Ry(-yaw) * Rx(-pitch)`.
fn euler_from_depth_edit(quat: Quat) -> Result<[f64; 3], String> {
    let quat = normalize_quat(quat);
    let m = quat_matrix(quat);
    let sine_pitch = (-m[2][0]).clamp(-1.0, 1.0);
    let ep = sine_pitch.asin();
    let (er, ey) = if ep.cos().abs() > 1e-6 {
        (m[2][1].atan2(m[2][2]), m[1][0].atan2(m[0][0]))
    } else if m[2][0] < 0.0 {
        // ep = +π/2. atan2(m01, m11) is the coupled roll; the opposite sign
        // reconstructs a different rotation.
        (m[0][1].atan2(m[1][1]), 0.0)
    } else {
        ((-m[0][1]).atan2(m[1][1]), 0.0)
    };
    let euler = [-er, -ep, ey];
    if euler.iter().any(|value| !value.is_finite()) {
        return Err("non-finite rotation".into());
    }
    Ok(euler)
}

fn normalize_quat(quat: Quat) -> Quat {
    let magnitude = (quat.w * quat.w + quat.x * quat.x + quat.y * quat.y + quat.z * quat.z).sqrt();
    if magnitude == 0.0 {
        quat
    } else {
        Quat {
            w: quat.w / magnitude,
            x: quat.x / magnitude,
            y: quat.y / magnitude,
            z: quat.z / magnitude,
        }
    }
}

fn inverse_quat(quat: Quat) -> Quat {
    Quat {
        w: quat.w,
        x: -quat.x,
        y: -quat.y,
        z: -quat.z,
    }
}

fn mul_quat(a: Quat, b: Quat) -> Quat {
    Quat {
        w: -a.x * b.x - a.y * b.y - a.z * b.z + a.w * b.w,
        x: a.x * b.w + a.y * b.z - a.z * b.y + a.w * b.x,
        y: -a.x * b.z + a.y * b.w + a.z * b.x + a.w * b.y,
        z: a.x * b.y - a.y * b.x + a.z * b.w + a.w * b.z,
    }
}

fn rotate_vec(quat: Quat, value: [f64; 3]) -> [f64; 3] {
    let ww = quat.w * quat.w;
    let w2 = quat.w * 2.0;
    let wx2 = w2 * quat.x;
    let wy2 = w2 * quat.y;
    let wz2 = w2 * quat.z;
    let xx = quat.x * quat.x;
    let x2 = quat.x * 2.0;
    let xy2 = x2 * quat.y;
    let xz2 = x2 * quat.z;
    let yy = quat.y * quat.y;
    let yz2 = 2.0 * quat.y * quat.z;
    let zz = quat.z * quat.z;
    [
        ww * value[0] + wy2 * value[2] - wz2 * value[1]
            + xx * value[0]
            + xy2 * value[1]
            + xz2 * value[2]
            - zz * value[0]
            - yy * value[0],
        xy2 * value[0] + yy * value[1] + yz2 * value[2] + wz2 * value[0] - zz * value[1]
            + ww * value[1]
            - wx2 * value[2]
            - xx * value[1],
        xz2 * value[0] + yz2 * value[1] + zz * value[2] - wy2 * value[0] - yy * value[2]
            + wx2 * value[1]
            - xx * value[2]
            + ww * value[2],
    ]
}

fn quat_matrix(quat: Quat) -> Mat4 {
    let xx = quat.x * quat.x;
    let xy = quat.x * quat.y;
    let xz = quat.x * quat.z;
    let xw = quat.x * quat.w;
    let yy = quat.y * quat.y;
    let yz = quat.y * quat.z;
    let yw = quat.y * quat.w;
    let zz = quat.z * quat.z;
    let zw = quat.z * quat.w;
    [
        [1.0 - 2.0 * (yy + zz), 2.0 * (xy - zw), 2.0 * (xz + yw), 0.0],
        [2.0 * (xy + zw), 1.0 - 2.0 * (xx + zz), 2.0 * (yz - xw), 0.0],
        [2.0 * (xz - yw), 2.0 * (yz + xw), 1.0 - 2.0 * (xx + yy), 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn translation_matrix(translation: [f64; 3]) -> Mat4 {
    [
        [1.0, 0.0, 0.0, translation[0]],
        [0.0, 1.0, 0.0, translation[1]],
        [0.0, 0.0, 1.0, translation[2]],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn scale_matrix(scale: [f64; 3]) -> Mat4 {
    [
        [scale[0], 0.0, 0.0, 0.0],
        [0.0, scale[1], 0.0, 0.0],
        [0.0, 0.0, scale[2], 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

fn compose(translation: [f64; 3], rotation: Quat) -> Mat4 {
    mul_mat(translation_matrix(translation), quat_matrix(rotation))
}

fn identity() -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

pub(crate) fn mul_mat(a: Mat4, b: Mat4) -> Mat4 {
    let mut result = [[0.0; 4]; 4];
    for row in 0..4 {
        for column in 0..4 {
            for k in 0..4 {
                result[row][column] += a[row][k] * b[k][column];
            }
        }
    }
    result
}

pub(crate) fn transform_point(matrix: Mat4, point: [f64; 3]) -> Result<[f64; 3], String> {
    let mut output = [0.0; 3];
    for row in 0..3 {
        output[row] = matrix[row][0] * point[0]
            + matrix[row][1] * point[1]
            + matrix[row][2] * point[2]
            + matrix[row][3];
    }
    if output.iter().any(|value| !value.is_finite()) {
        return Err("non-finite matrix".into());
    }
    Ok(output)
}

fn matrix_finite(matrix: Mat4) -> bool {
    matrix.iter().flatten().all(|value| value.is_finite())
}

pub(crate) fn invert(matrix: Mat4) -> Result<Mat4, String> {
    if !matrix_finite(matrix) {
        return Err("non-finite matrix".into());
    }
    let mut rows = matrix;
    let mut inverse = identity();
    for col in 0..4 {
        let mut pivot = col;
        let mut best = rows[col][col].abs();
        for row in (col + 1)..4 {
            let value = rows[row][col].abs();
            if value > best {
                best = value;
                pivot = row;
            }
        }
        if !best.is_finite() || best < 1e-12 {
            return Err("non-finite matrix".into());
        }
        if pivot != col {
            rows.swap(pivot, col);
            inverse.swap(pivot, col);
        }
        let divisor = rows[col][col];
        for column in 0..4 {
            rows[col][column] /= divisor;
            inverse[col][column] /= divisor;
        }
        for row in 0..4 {
            if row == col {
                continue;
            }
            let factor = rows[row][col];
            for column in 0..4 {
                rows[row][column] -= factor * rows[col][column];
                inverse[row][column] -= factor * inverse[col][column];
            }
        }
    }
    if !matrix_finite(inverse) {
        return Err("non-finite matrix".into());
    }
    Ok(inverse)
}

pub(crate) fn overlay_geometry(
    model: &Value,
    root_id: u32,
    selected: Option<u32>,
) -> Result<Value, String> {
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    if let Some(selected) = selected {
        let bone = nodes::find_node(model, selected)
            .ok_or_else(|| format!("node {selected} was not found"))?;
        if bone.get("type").and_then(Value::as_str) != Some("DepthBone")
            || !bone_order(model, root_id)?.contains(&selected)
        {
            return Err("selected bone is not in this rig".into());
        }
    }
    let root_inverse = invert(world_matrix(model, root_id)?)?;
    let mut points = Vec::new();
    let mut selected_points = Vec::new();
    let mut lines = Vec::new();
    let mut parent_lines = Vec::new();
    let mut child_lines = Vec::new();
    for bone_id in bone_order(model, root_id)? {
        let point = rig_local_origin(model, root_inverse, bone_id)?;
        if selected == Some(bone_id) {
            selected_points.push(point);
        } else {
            points.push(point);
        }
        let Some(parent_id) = parent_of(model, bone_id)? else {
            continue;
        };
        let parent = nodes::find_node(model, parent_id)
            .ok_or_else(|| format!("node {parent_id} was not found"))?;
        if parent.get("type").and_then(Value::as_str) != Some("DepthBone") {
            continue;
        }
        let parent_point = rig_local_origin(model, root_inverse, parent_id)?;
        let line = json!([point3(parent_point), point3(point)]);
        if selected == Some(bone_id) {
            parent_lines.push(line);
        } else if selected == Some(parent_id) {
            child_lines.push(line);
        } else {
            lines.push(line);
        }
    }
    let effective = if let Some(selected) = selected {
        effective_pivots(model, root_id, selected)?
    } else {
        Vec::new()
    };
    Ok(json!({
        "points": points.into_iter().map(point3).collect::<Vec<_>>(),
        "selectedPoints": selected_points.into_iter().map(point3).collect::<Vec<_>>(),
        "lines": lines,
        "parentLines": parent_lines,
        "childLines": child_lines,
        "effectivePivots": effective,
    }))
}

const TRANSLATION_CHANNELS: [&str; 3] = ["transform.t.x", "transform.t.y", "transform.t.z"];
const ROTATION_CHANNELS: [&str; 3] = ["transform.r.x", "transform.r.y", "transform.r.z"];

pub(crate) fn pick_bone(
    model: &Value,
    root_id: u32,
    point: &Value,
    radius: f64,
) -> Result<Value, String> {
    require_rig_root(model, root_id)?;
    let point = finite_vec3(point, "point")?;
    if !radius.is_finite() || radius < 0.0 {
        return Err("radius must be a finite non-negative number".into());
    }
    let root_inverse = invert(world_matrix(model, root_id)?)?;
    let mut best: Option<(u32, f64)> = None;
    for bone_id in bone_order(model, root_id)? {
        let origin = rig_local_origin(model, root_inverse, bone_id)?;
        let dist = distance(origin, point);
        let closer = best.is_none_or(|(_, best_dist)| dist < best_dist);
        if dist <= radius && closer {
            best = Some((bone_id, dist));
        }
    }
    Ok(json!({ "bone": best.map(|(id, _)| id) }))
}

pub(crate) fn require_rig_bone(model: &Value, root_id: u32, bone_id: u32) -> Result<(), String> {
    require_rig_root(model, root_id)?;
    let bone = nodes::find_node(model, bone_id)
        .ok_or_else(|| format!("node {bone_id} was not found"))?;
    if bone.get("type").and_then(Value::as_str) != Some("DepthBone")
        || !bone_order(model, root_id)?.contains(&bone_id)
    {
        return Err("bone is not in this rig".into());
    }
    Ok(())
}

/// A driven drag leaves the base transform unchanged. Evaluation adds the
/// parameter channel to that base, so the closest key stores
/// `requested - base`. Other keys keep their offsets from the same base.
pub(crate) fn apply_drag_pose(
    model: &mut Value,
    root_id: u32,
    bone_id: u32,
    values: &BTreeMap<u32, [f64; 2]>,
    translation_axes: Option<[bool; 3]>,
    rotation_axes: Option<[bool; 3]>,
    base_translation: [f64; 3],
    base_rotation: [f64; 3],
) -> Result<(), String> {
    if (translation_axes.is_none() && rotation_axes.is_none()) || !rig_has_driver(model, root_id)? {
        return Ok(());
    }
    let node = nodes::find_node(model, bone_id)
        .ok_or_else(|| format!("node {bone_id} was not found"))?;
    let desired_translation = transform_vec(node, "trans", [0.0, 0.0, 0.0]);
    let desired_rotation = transform_vec(node, "rot", [0.0, 0.0, 0.0]);
    if let Some(axes) = translation_axes {
        let parameter = sole_channel_parameter(model, bone_id, &TRANSLATION_CHANNELS, "translation")?;
        let delta = sub(desired_translation, base_translation);
        restore_transform_vec(model, bone_id, "trans", base_translation)?;
        write_channel_group(
            model,
            values,
            parameter,
            bone_id,
            &TRANSLATION_CHANNELS,
            &delta,
            axes,
        )?;
    }
    if let Some(axes) = rotation_axes {
        let parameter = sole_channel_parameter(model, bone_id, &ROTATION_CHANNELS, "rotation")?;
        let delta = sub(desired_rotation, base_rotation);
        restore_transform_vec(model, bone_id, "rot", base_rotation)?;
        write_channel_group(
            model,
            values,
            parameter,
            bone_id,
            &ROTATION_CHANNELS,
            &delta,
            axes,
        )?;
    }
    Ok(())
}

const IK_ITERATIONS: usize = 16;

pub(crate) fn solve_ik(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    effector_id: u32,
    target: &Value,
) -> Result<(Value, Value), String> {
    require_rig_bone(model, root_id, effector_id)?;
    let target = finite_vec3(target, "target")?;
    let chain = ik_chain(model, root_id, effector_id)?;
    let movable: Vec<u32> = chain
        .iter()
        .copied()
        .filter(|id| {
            nodes::find_node(model, *id).is_none_or(|node| !flag(node, "lockRotation"))
        })
        .collect();
    if movable.is_empty() {
        return Err("IK chain has no movable bone".into());
    }
    let driven = rig_has_driver(model, root_id)?;
    if driven {
        for bone_id in &movable {
            sole_channel_parameter(model, *bone_id, &ROTATION_CHANNELS, "rotation")?;
        }
    }
    for bone_id in &movable {
        let node = nodes::find_node(model, *bone_id)
            .ok_or_else(|| format!("node {bone_id} was not found"))?;
        let _ = hinge_axis(node)?;
        let _ = hinge_limits(node)?;
        let _ = hinge_step(node)?;
    }
    let mut bones = runtime_for_ik(model, values, root_id)?;
    let end_index = bones
        .iter()
        .position(|bone| bone.id == effector_id)
        .ok_or("bone is not in this rig")?;
    let distance_before = distance(posed_tail(&bones[end_index]), target);
    let mut previous = posed_tail(&bones[end_index]);
    for _ in 0..IK_ITERATIONS {
        for bone_id in &chain {
            let node = nodes::find_node(model, *bone_id)
                .ok_or_else(|| format!("node {bone_id} was not found"))?;
            if flag(node, "lockRotation") {
                continue;
            }
            let index = bones
                .iter()
                .position(|bone| bone.id == *bone_id)
                .ok_or("bone is not in this rig")?;
            let axis = hinge_axis(node)?;
            let limits = hinge_limits(node)?;
            let step_cap = hinge_step(node)?;
            let world_axis = normalize3(rotate_vec(bones[index].world_prefix, axis));
            if distance(world_axis, [0.0; 3]) < 1e-8 {
                return Err("hingeAxis must be a non-zero vector".into());
            }
            let pivot = bones[index].world_head;
            let effector = posed_tail(&bones[end_index]);
            let mut delta = signed_angle(sub(effector, pivot), sub(target, pivot), world_axis);
            if let Some(cap) = step_cap {
                delta = delta.clamp(-cap, cap);
            }
            let current = twist_angle(bones[index].pose_quat, axis);
            let mut next = current + delta;
            if let Some((min, max)) = limits {
                next = next.clamp(min.min(max), min.max(max));
            }
            let applied = next - current;
            if applied.abs() < 1e-12 {
                continue;
            }
            bones[index].pose_quat = normalize_quat(mul_quat(
                axis_rotation(applied, axis),
                bones[index].pose_quat,
            ));
            rebuild_pose_world(&mut bones);
        }
        let tail = posed_tail(&bones[end_index]);
        if distance(tail, previous) < 1e-4 {
            break;
        }
        previous = tail;
    }
    let mut solved = BTreeMap::new();
    for bone_id in &movable {
        let index = bones
            .iter()
            .position(|bone| bone.id == *bone_id)
            .ok_or("bone is not in this rig")?;
        solved.insert(*bone_id, euler_from_depth_edit(bones[index].pose_quat)?);
    }
    let mut candidate = model.clone();
    for bone_id in &movable {
        let solved_euler = solved[bone_id];
        let base = transform_vec(
            nodes::find_node(model, *bone_id).ok_or_else(|| format!("node {bone_id} was not found"))?,
            "rot",
            [0.0, 0.0, 0.0],
        );
        restore_transform_vec(&mut candidate, *bone_id, "rot", solved_euler)?;
        apply_drag_pose(
            &mut candidate,
            root_id,
            *bone_id,
            values,
            None,
            Some([true, true, true]),
            [0.0; 3],
            base,
        )?;
    }
    let after_bones = runtime_for_ik(&candidate, values, root_id)?;
    let after_index = after_bones
        .iter()
        .position(|bone| bone.id == effector_id)
        .ok_or("bone is not in this rig")?;
    let distance_after = distance(posed_tail(&after_bones[after_index]), target);
    if !distance_after.is_finite() {
        return Err("non-finite rotation".into());
    }
    Ok((
        candidate,
        json!({
            "endEffector": effector_id,
            "distanceBefore": distance_before,
            "distanceAfter": distance_after,
        }),
    ))
}

/// Reflect selected bones through the rig-local YZ plane.
///
/// Rest points use `(x, y, z) -> (-x, y, z)` and `restRoll` changes sign.
/// Pose is the reflected rig-space world, converted back to a local transform
/// under the parent after that parent has been reflected. A driven rig stores
/// that pose with the same channel write as `rig.solve_ik` and `rig.drag_bone`.
pub(crate) fn mirror_bones(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    bones: &Value,
    axis: &Value,
) -> Result<Value, String> {
    require_rig_root(model, root_id)?;
    if axis.as_str() != Some("x") {
        return Err("axis must be x".into());
    }
    let requested = bone_id_list(bones)?;
    let mut selected = BTreeSet::new();
    for id in requested {
        require_rig_bone(model, root_id, id)?;
        selected.insert(id);
    }
    require_closed_subtree(model, &selected)?;
    for id in &selected {
        require_finite_rest(model, *id)?;
    }
    let weight_plans = mirrored_weight_rows(model, values, root_id, &selected)?;
    let driven = rig_has_driver(model, root_id)?;
    if driven {
        for id in &selected {
            sole_channel_parameter(model, *id, &TRANSLATION_CHANNELS, "translation")?;
            sole_channel_parameter(model, *id, &ROTATION_CHANNELS, "rotation")?;
        }
    }
    let updates = reflected_pose_updates(model, values, root_id, &selected)?;
    let mut candidate = model.clone();
    for update in &updates {
        let node = nodes::find_node_mut(&mut candidate, update.id)
            .ok_or_else(|| format!("node {} was not found", update.id))?;
        let object = node.as_object_mut().ok_or("node must be an object")?;
        object.insert("restHead".into(), json!(update.rest_head));
        object.insert("restTail".into(), json!(update.rest_tail));
        object.insert("restRoll".into(), json!(update.rest_roll));
        restore_transform_vec(&mut candidate, update.id, "trans", update.translation)?;
        restore_transform_vec(&mut candidate, update.id, "rot", update.rotation)?;
        apply_drag_pose(
            &mut candidate,
            root_id,
            update.id,
            values,
            Some([true, true, true]),
            Some([true, true, true]),
            update.base_translation,
            update.base_rotation,
        )?;
    }
    for (target_id, rows) in weight_plans {
        let root = nodes::find_node_mut(&mut candidate, root_id)
            .ok_or_else(|| format!("node {root_id} was not found"))?;
        let bindings = root
            .get_mut("bindings")
            .and_then(Value::as_array_mut)
            .ok_or("bindings must be an array")?;
        let binding = bindings
            .iter_mut()
            .find(|binding| {
                binding
                    .get("target")
                    .and_then(Value::as_u64)
                    .and_then(valid_id)
                    == Some(target_id)
            })
            .ok_or("Depth rig binding is required")?;
        binding
            .as_object_mut()
            .ok_or("binding must be an object")?
            .insert("vertexWeights".into(), json!(rows));
    }
    Ok(candidate)
}

struct PoseUpdate {
    id: u32,
    translation: [f64; 3],
    rotation: [f64; 3],
    base_translation: [f64; 3],
    base_rotation: [f64; 3],
    rest_head: [f64; 3],
    rest_tail: [f64; 3],
    rest_roll: f64,
}

fn reflected_pose_updates(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    selected: &BTreeSet<u32>,
) -> Result<Vec<PoseUpdate>, String> {
    let posed = posed_model(model, values)?;
    let root_world = world_matrix(&posed, root_id)?;
    let root_inverse = invert(root_world)?;
    let order = bone_order(model, root_id)?;
    let mut rig_world = BTreeMap::new();
    for id in &order {
        let puppet = world_matrix(&posed, *id)?;
        rig_world.insert(*id, mul_mat(root_inverse, puppet));
    }
    let mut desired_puppet = BTreeMap::new();
    for id in order.iter().filter(|id| selected.contains(id)) {
        let mirrored = mirror_x_matrix(rig_world[id]);
        desired_puppet.insert(*id, mul_mat(root_world, mirrored));
    }
    let mut updates = Vec::new();
    for id in order.into_iter().filter(|id| selected.contains(id)) {
        let basis = composition_basis(&posed, id, selected, &desired_puppet)?;
        let local = mul_mat(invert(basis)?, desired_puppet[&id]);
        let node = nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
        let posed_node =
            nodes::find_node(&posed, id).ok_or_else(|| format!("node {id} was not found"))?;
        let scale = transform_vec(posed_node, "scale", [1.0, 1.0, 1.0]);
        let (translation, rotation) = decompose_trs(local, scale)?;
        let (rest_head, rest_tail, rest_roll) = stored_rest(node)?;
        updates.push(PoseUpdate {
            id,
            translation: canonical_vec(translation),
            rotation: canonical_vec(rotation),
            base_translation: transform_vec(node, "trans", [0.0, 0.0, 0.0]),
            base_rotation: transform_vec(node, "rot", [0.0, 0.0, 0.0]),
            rest_head: canonical_vec(mirror_point(rest_head)),
            rest_tail: canonical_vec(mirror_point(rest_tail)),
            rest_roll: canonical_zero(-rest_roll),
        });
    }
    Ok(updates)
}

fn composition_basis(
    model: &Value,
    id: u32,
    selected: &BTreeSet<u32>,
    desired_puppet: &BTreeMap<u32, Mat4>,
) -> Result<Mat4, String> {
    let node = nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    if flag(node, "lockToRoot") {
        let top = chain_root(model, id)?;
        if top == id {
            return Ok(identity());
        }
        let root =
            nodes::find_node(model, top).ok_or_else(|| format!("node {top} was not found"))?;
        return local_matrix(root);
    }
    match parent_of(model, id)? {
        Some(parent) if selected.contains(&parent) => desired_puppet
            .get(&parent)
            .copied()
            .ok_or_else(|| "non-finite matrix".to_string()),
        Some(parent) => world_matrix(model, parent),
        None => Ok(identity()),
    }
}

fn chain_root(model: &Value, id: u32) -> Result<u32, String> {
    let mut current = id;
    for _ in 0..4096 {
        match parent_of(model, current)? {
            Some(parent) => current = parent,
            None => return Ok(current),
        }
    }
    Err("node was not found".into())
}

fn bone_id_list(value: &Value) -> Result<Vec<u32>, String> {
    let items = value
        .as_array()
        .ok_or("bones must be an array of bone UUIDs")?;
    if items.is_empty() {
        return Err("bones must list one or more bones".into());
    }
    let mut ids = Vec::with_capacity(items.len());
    for item in items {
        let id = item
            .as_u64()
            .and_then(valid_id)
            .ok_or("bones must be an array of bone UUIDs")?;
        ids.push(id);
    }
    Ok(ids)
}

fn require_closed_subtree(model: &Value, selected: &BTreeSet<u32>) -> Result<(), String> {
    for id in selected {
        let node =
            nodes::find_node(model, *id).ok_or_else(|| format!("node {id} was not found"))?;
        require_descendants_selected(node, selected)?;
    }
    Ok(())
}

fn require_descendants_selected(node: &Value, selected: &BTreeSet<u32>) -> Result<(), String> {
    let Some(children) = node.get("children").and_then(Value::as_array) else {
        return Ok(());
    };
    for child in children {
        if child.get("type").and_then(Value::as_str) == Some("DepthBone") {
            let id = child
                .get("uuid")
                .and_then(Value::as_u64)
                .and_then(valid_id)
                .ok_or("node is missing uuid")?;
            if !selected.contains(&id) {
                return Err("selection is not a closed subtree".into());
            }
        }
        require_descendants_selected(child, selected)?;
    }
    Ok(())
}

fn require_finite_rest(model: &Value, id: u32) -> Result<(), String> {
    let node = nodes::find_node(model, id).ok_or_else(|| format!("node {id} was not found"))?;
    let _ = stored_rest(node)?;
    Ok(())
}

fn finite_rest_vec(node: &Value, key: &str) -> Result<[f64; 3], String> {
    let value = node.get(key).ok_or("non-finite rest point")?;
    finite_vec3(value, key).map_err(|_| "non-finite rest point".to_string())
}

fn stored_rest(node: &Value) -> Result<([f64; 3], [f64; 3], f64), String> {
    let head = finite_rest_vec(node, "restHead")?;
    let tail = finite_rest_vec(node, "restTail")?;
    let roll = match node.get("restRoll") {
        None | Some(Value::Null) => 0.0,
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or("non-finite rest point")?,
    };
    Ok((head, tail, roll))
}

fn mirrored_weight_rows(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
    selected: &BTreeSet<u32>,
) -> Result<Vec<(u32, Vec<Vec<f64>>)>, String> {
    let posed = posed_model(model, values)?;
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let Some(bindings) = root.get("bindings").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut plans = Vec::new();
    for binding in bindings {
        if binding.get("vertexWeights").is_none() {
            continue;
        }
        let rows = binding
            .get("vertexWeights")
            .and_then(Value::as_array)
            .ok_or("vertexWeights must be an array")?;
        let sources = binding
            .get("sourceBoneUuids")
            .and_then(Value::as_array)
            .ok_or("sourceBoneUuids must be an array")?;
        if rows.len() != sources.len() {
            return Err("vertexWeights length must match the source list".into());
        }
        for source in sources {
            let id = source
                .as_u64()
                .and_then(valid_id)
                .ok_or("weighted binding sources are not all selected")?;
            if !selected.contains(&id) {
                return Err("weighted binding sources are not all selected".into());
            }
        }
        let target_id = binding
            .get("target")
            .and_then(Value::as_u64)
            .and_then(valid_id)
            .ok_or("Depth rig binding is required")?;
        let target = nodes::find_node(&posed, target_id)
            .ok_or_else(|| format!("node {target_id} was not found"))?;
        let vertices = target_vertices(target)?;
        // Deform lifts each vertex through `target_to_root`. Pair in that
        // rig space so a target translation or rotation cannot treat a local
        // `[-x, y]` neighbor as the mirror partner.
        let to_root = target_to_root(model, &posed, root_id, target_id)?;
        let mut rig_points = Vec::with_capacity(vertices.len());
        for vertex in &vertices {
            rig_points.push(transform_point(to_root, [vertex[0], vertex[1], 0.0])?);
        }
        let partner = pair_mirrored_vertices(&rig_points)?;
        let mut permuted = Vec::with_capacity(rows.len());
        for row in rows {
            let row = row.as_array().ok_or("vertex weight row must be an array")?;
            if row.len() != vertices.len() {
                return Err("vertex weight length must match the vertex count".into());
            }
            let mut next = vec![0.0; row.len()];
            for (index, value) in row.iter().enumerate() {
                let weight = value
                    .as_f64()
                    .filter(|number| number.is_finite() && *number >= 0.0)
                    .ok_or("vertex weight must be finite and >= 0")?;
                next[partner[index]] = weight;
            }
            permuted.push(next);
        }
        plans.push((target_id, permuted));
    }
    Ok(plans)
}

fn pair_mirrored_vertices(vertices: &[[f64; 3]]) -> Result<Vec<usize>, String> {
    let mut partner = vec![0; vertices.len()];
    for (index, vertex) in vertices.iter().enumerate() {
        let mirrored = mirror_point(*vertex);
        let mut nearest: Option<(usize, f64)> = None;
        for (other, point) in vertices.iter().enumerate() {
            let delta = sub(*point, mirrored);
            let dist = (delta[0] * delta[0] + delta[1] * delta[1] + delta[2] * delta[2]).sqrt();
            if dist > 1e-4 {
                continue;
            }
            match nearest {
                Some((chosen, best)) if (dist - best).abs() <= 1e-9 && chosen != other => {
                    return Err("unpaired weighted vertex".into());
                }
                Some((_, best)) if dist + 1e-9 < best => nearest = Some((other, dist)),
                None => nearest = Some((other, dist)),
                _ => {}
            }
        }
        partner[index] = nearest.ok_or("unpaired weighted vertex")?.0;
    }
    for (index, chosen) in partner.iter().copied().enumerate() {
        if partner.get(chosen).copied() != Some(index) {
            return Err("unpaired weighted vertex".into());
        }
    }
    Ok(partner)
}

fn mirror_point(point: [f64; 3]) -> [f64; 3] {
    [-point[0], point[1], point[2]]
}

fn canonical_zero(value: f64) -> f64 {
    if value == 0.0 { 0.0 } else { value }
}

fn canonical_vec(value: [f64; 3]) -> [f64; 3] {
    [canonical_zero(value[0]), canonical_zero(value[1]), canonical_zero(value[2])]
}

fn mirror_x_matrix(matrix: Mat4) -> Mat4 {
    let mirror = [
        [-1.0, 0.0, 0.0, 0.0],
        [0.0, 1.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ];
    mul_mat(mirror, mul_mat(matrix, mirror))
}

fn decompose_trs(matrix: Mat4, scale: [f64; 3]) -> Result<([f64; 3], [f64; 3]), String> {
    if !matrix_finite(matrix) || scale.iter().any(|value| !value.is_finite()) {
        return Err("non-finite matrix".into());
    }
    if scale[0].abs() < 1e-12 || scale[1].abs() < 1e-12 || scale[2].abs() < 1e-12 {
        return Err("non-finite matrix".into());
    }
    let translation = [matrix[0][3], matrix[1][3], matrix[2][3]];
    let mut rotation = identity();
    for row in 0..3 {
        rotation[row][0] = matrix[row][0] / scale[0];
        rotation[row][1] = matrix[row][1] / scale[1];
        rotation[row][2] = matrix[row][2] / scale[2];
    }
    // A rotated parent with non-uniform scale can turn the reflected world
    // into a shear. Dividing by the retained scale must leave a rotation.
    if !is_proper_rotation(&rotation) {
        return Err("mirrored local transform is not representable".into());
    }
    let euler = euler_from_node_rotation(quat_from_matrix(rotation)?)?;
    if translation
        .iter()
        .chain(euler.iter())
        .any(|value| !value.is_finite())
    {
        return Err("non-finite rotation".into());
    }
    Ok((translation, euler))
}

fn is_proper_rotation(matrix: &Mat4) -> bool {
    const EPSILON: f64 = 1e-6;
    let columns = [
        [matrix[0][0], matrix[1][0], matrix[2][0]],
        [matrix[0][1], matrix[1][1], matrix[2][1]],
        [matrix[0][2], matrix[1][2], matrix[2][2]],
    ];
    for column in columns {
        let length = (column[0] * column[0] + column[1] * column[1] + column[2] * column[2]).sqrt();
        if (length - 1.0).abs() > EPSILON {
            return false;
        }
    }
    for pair in [(0, 1), (0, 2), (1, 2)] {
        let left = columns[pair.0];
        let right = columns[pair.1];
        let dot = left[0] * right[0] + left[1] * right[1] + left[2] * right[2];
        if dot.abs() > EPSILON {
            return false;
        }
    }
    let det = columns[0][0] * (columns[1][1] * columns[2][2] - columns[1][2] * columns[2][1])
        - columns[0][1] * (columns[1][0] * columns[2][2] - columns[1][2] * columns[2][0])
        + columns[0][2] * (columns[1][0] * columns[2][1] - columns[1][1] * columns[2][0]);
    (det - 1.0).abs() <= EPSILON
}

fn quat_from_matrix(matrix: Mat4) -> Result<Quat, String> {
    let m00 = matrix[0][0];
    let m01 = matrix[0][1];
    let m02 = matrix[0][2];
    let m10 = matrix[1][0];
    let m11 = matrix[1][1];
    let m12 = matrix[1][2];
    let m20 = matrix[2][0];
    let m21 = matrix[2][1];
    let m22 = matrix[2][2];
    let trace = m00 + m11 + m22;
    let quat = if trace > 0.0 {
        let scale = (trace + 1.0).sqrt() * 2.0;
        Quat {
            w: 0.25 * scale,
            x: (m21 - m12) / scale,
            y: (m02 - m20) / scale,
            z: (m10 - m01) / scale,
        }
    } else if m00 > m11 && m00 > m22 {
        let scale = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        Quat {
            w: (m21 - m12) / scale,
            x: 0.25 * scale,
            y: (m01 + m10) / scale,
            z: (m02 + m20) / scale,
        }
    } else if m11 > m22 {
        let scale = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        Quat {
            w: (m02 - m20) / scale,
            x: (m01 + m10) / scale,
            y: 0.25 * scale,
            z: (m12 + m21) / scale,
        }
    } else {
        let scale = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        Quat {
            w: (m10 - m01) / scale,
            x: (m02 + m20) / scale,
            y: (m12 + m21) / scale,
            z: 0.25 * scale,
        }
    };
    if ![quat.w, quat.x, quat.y, quat.z]
        .iter()
        .all(|value| value.is_finite())
    {
        return Err("non-finite rotation".into());
    }
    Ok(normalize_quat(quat))
}

fn euler_from_node_rotation(quat: Quat) -> Result<[f64; 3], String> {
    let depth = euler_from_depth_edit(quat)?;
    let euler = [-depth[0], -depth[1], depth[2]];
    if euler.iter().any(|value| !value.is_finite()) {
        return Err("non-finite rotation".into());
    }
    Ok(euler)
}

fn runtime_for_ik(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    root_id: u32,
) -> Result<Vec<BoneRuntime>, String> {
    let mut bones = build_runtime(model, root_id, None, None)?;
    if !rig_has_driver(model, root_id)? {
        return Ok(bones);
    }
    for index in 0..bones.len() {
        let bone_id = bones[index].id;
        let node = nodes::find_node(model, bone_id)
            .ok_or_else(|| format!("node {bone_id} was not found"))?;
        let rotation = if flag(node, "lockRotation") {
            None
        } else {
            sole_channel_parameter(model, bone_id, &ROTATION_CHANNELS, "rotation").ok()
        };
        let translation = if flag(node, "lockTranslation") {
            Some([0.0; 3])
        } else if keyed_translation(model, bone_id)? {
            Some([
                keyed_channel(model, values, bone_id, "transform.t.x")?,
                keyed_channel(model, values, bone_id, "transform.t.y")?,
                keyed_channel(model, values, bone_id, "transform.t.z")?,
            ])
        } else {
            None
        };
        if rotation.is_none() && translation.is_none() {
            continue;
        }
        if let Some(parameter) = rotation {
            let key = closest_keypoint(model, values, parameter)?;
            let pitch = binding_scalar(model, parameter, bone_id, "transform.r.x", key)?;
            let yaw = binding_scalar(model, parameter, bone_id, "transform.r.y", key)?;
            let roll = binding_scalar(model, parameter, bone_id, "transform.r.z", key)?;
            bones[index].pose_yaw = yaw;
            bones[index].pose_quat = depth_edit_rotation(pitch, yaw, roll);
            bones[index].pose_without_yaw = depth_edit_rotation(pitch, 0.0, roll);
        } else if flag(node, "lockRotation") {
            bones[index].pose_yaw = 0.0;
            bones[index].pose_quat = identity_quat();
            bones[index].pose_without_yaw = identity_quat();
        }
        if let Some(translation) = translation {
            bones[index].pose_translation = translation;
        }
    }
    rebuild_pose_world(&mut bones);
    Ok(bones)
}

fn keyed_translation(model: &Value, bone_id: u32) -> Result<bool, String> {
    for channel in TRANSLATION_CHANNELS {
        if !channel_owners(model, bone_id, channel)?.is_empty() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn keyed_channel(
    model: &Value,
    values: &BTreeMap<u32, [f64; 2]>,
    bone_id: u32,
    channel: &str,
) -> Result<f64, String> {
    let mut total = 0.0;
    for parameter in channel_owners(model, bone_id, channel)? {
        let key = closest_keypoint(model, values, parameter)?;
        total += binding_scalar(model, parameter, bone_id, channel, key)?;
    }
    Ok(total)
}

fn ik_chain(model: &Value, root_id: u32, effector_id: u32) -> Result<Vec<u32>, String> {
    let mut chain = Vec::new();
    let mut current = effector_id;
    for _ in 0..4096 {
        if current == root_id {
            break;
        }
        let node = nodes::find_node(model, current)
            .ok_or_else(|| format!("node {current} was not found"))?;
        if node.get("type").and_then(Value::as_str) != Some("DepthBone")
            || !bone_order(model, root_id)?.contains(&current)
        {
            break;
        }
        if flag(node, "lockToRoot") {
            break;
        }
        chain.push(current);
        match parent_of(model, current)? {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Ok(chain)
}

fn hinge_axis(node: &Value) -> Result<[f64; 3], String> {
    match node.get("hingeAxis") {
        None | Some(Value::Null) => Ok([0.0, 0.0, 1.0]),
        Some(value) => {
            let axis = finite_vec3(value, "hingeAxis")?;
            if distance(axis, [0.0; 3]) < 1e-8 {
                return Err("hingeAxis must be a non-zero vector".into());
            }
            Ok(normalize3(axis))
        }
    }
}

fn hinge_limits(node: &Value) -> Result<Option<(f64, f64)>, String> {
    match node.get("rotationLimits") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let items = value
                .as_array()
                .ok_or("rotationLimits must be [min, max]")?;
            if items.is_empty() {
                return Ok(None);
            }
            if items.len() != 2 {
                return Err("rotationLimits must be [min, max]".into());
            }
            let min = items[0]
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or("rotationLimits must be [min, max]")?;
            let max = items[1]
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or("rotationLimits must be [min, max]")?;
            Ok(Some((min, max)))
        }
    }
}

fn hinge_step(node: &Value) -> Result<Option<f64>, String> {
    match node.get("maxStepRadians") {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let step = value
                .as_f64()
                .filter(|n| n.is_finite())
                .ok_or("maxStepRadians must be finite")?;
            Ok(Some(step.abs()))
        }
    }
}

fn posed_tail(bone: &BoneRuntime) -> [f64; 3] {
    add(
        bone.world_head,
        rotate_vec(bone.world_quat, [0.0, bone.rest_length, 0.0]),
    )
}

fn rebuild_pose_world(bones: &mut [BoneRuntime]) {
    for index in 0..bones.len() {
        let (world_head, world_prefix) = if bones[index].parent_index.is_some()
            && bones[index].allow_parent
            && !bones[index].lock_to_root
        {
            let parent = bones[index].parent_index.unwrap();
            let parent_quat = bones[parent].world_quat;
            let head = add(
                bones[parent].world_head,
                add(
                    rotate_vec(parent_quat, bones[index].local_rest_offset),
                    rotate_vec(parent_quat, bones[index].pose_translation),
                ),
            );
            (head, mul_quat(parent_quat, bones[index].local_rest_quat))
        } else {
            (
                add(bones[index].rest_head, bones[index].pose_translation),
                bones[index].rest_quat,
            )
        };
        bones[index].world_head = world_head;
        bones[index].world_prefix = world_prefix;
        bones[index].world_quat = mul_quat(world_prefix, bones[index].pose_quat);
    }
}

fn signed_angle(from: [f64; 3], to: [f64; 3], axis: [f64; 3]) -> f64 {
    let from = sub(from, scale3(axis, dot(from, axis)));
    let to = sub(to, scale3(axis, dot(to, axis)));
    let from_len = distance(from, [0.0; 3]);
    let to_len = distance(to, [0.0; 3]);
    if from_len < 1e-8 || to_len < 1e-8 {
        return 0.0;
    }
    let from = scale3(from, 1.0 / from_len);
    let to = scale3(to, 1.0 / to_len);
    let cosine = dot(from, to).clamp(-1.0, 1.0);
    let sine = dot(axis, cross(from, to));
    sine.atan2(cosine)
}

fn twist_angle(quat: Quat, axis: [f64; 3]) -> f64 {
    let projection = quat.x * axis[0] + quat.y * axis[1] + quat.z * axis[2];
    let mut twist = Quat {
        w: quat.w,
        x: axis[0] * projection,
        y: axis[1] * projection,
        z: axis[2] * projection,
    };
    let magnitude = (twist.w * twist.w + twist.x * twist.x + twist.y * twist.y + twist.z * twist.z).sqrt();
    if magnitude < 1e-12 {
        return 0.0;
    }
    twist.w /= magnitude;
    twist.x /= magnitude;
    twist.y /= magnitude;
    twist.z /= magnitude;
    let sine = twist.x * axis[0] + twist.y * axis[1] + twist.z * axis[2];
    2.0 * sine.atan2(twist.w)
}

fn restore_transform_vec(model: &mut Value, bone_id: u32, key: &str, value: [f64; 3]) -> Result<(), String> {
    let node = nodes::find_node_mut(model, bone_id)
        .ok_or_else(|| format!("node {bone_id} was not found"))?;
    node.get_mut("transform")
        .and_then(Value::as_object_mut)
        .ok_or("node transform must be an object")?
        .insert(key.to_string(), json!(value));
    Ok(())
}

pub(crate) fn require_rig_root(model: &Value, root_id: u32) -> Result<(), String> {
    let root = nodes::find_node(model, root_id)
        .ok_or_else(|| format!("node {root_id} was not found"))?;
    if root.get("type").and_then(Value::as_str) != Some("DepthRigRoot") {
        return Err("Node is not a DepthRigRoot".into());
    }
    Ok(())
}

fn finite_vec3(value: &Value, name: &str) -> Result<[f64; 3], String> {
    let values = value
        .as_array()
        .ok_or_else(|| format!("{name} must be a finite 3-vector"))?;
    if values.len() != 3 {
        return Err(format!("{name} must be a finite 3-vector"));
    }
    let mut point = [0.0; 3];
    for (index, item) in values.iter().enumerate() {
        point[index] = item
            .as_f64()
            .filter(|number| number.is_finite())
            .ok_or_else(|| format!("{name} must be a finite 3-vector"))?;
    }
    Ok(point)
}

fn rig_has_driver(model: &Value, root_id: u32) -> Result<bool, String> {
    for parameter in parameter_ids(model) {
        if parameter_drives_rig(model, root_id, parameter)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn sole_channel_parameter(
    model: &Value,
    bone_id: u32,
    channels: &[&str],
    kind: &str,
) -> Result<u32, String> {
    let mut owner: Option<u32> = None;
    for channel in channels {
        let owners = channel_owners(model, bone_id, channel)?;
        if owners.len() != 1 {
            return Err(format!("bone {bone_id} has no single {kind} parameter"));
        }
        if let Some(current) = owner {
            if current != owners[0] {
                return Err(format!("bone {bone_id} has no single {kind} parameter"));
            }
        } else {
            owner = Some(owners[0]);
        }
    }
    owner.ok_or_else(|| format!("bone {bone_id} has no single {kind} parameter"))
}

fn channel_owners(model: &Value, bone_id: u32, channel: &str) -> Result<Vec<u32>, String> {
    let mut owners = Vec::new();
    for parameter_id in parameter_ids(model) {
        let Some(parameter) = find_parameter(model, parameter_id) else {
            continue;
        };
        let Some(bindings) = parameter.get("bindings").and_then(Value::as_array) else {
            continue;
        };
        let count = bindings
            .iter()
            .filter(|binding| {
                binding.get("node").and_then(Value::as_u64).and_then(valid_id) == Some(bone_id)
                    && binding.get("param_name").and_then(Value::as_str) == Some(channel)
            })
            .count();
        if count > 1 {
            return Err(format!("bone {bone_id} has duplicate {channel} bindings"));
        }
        if count == 1 {
            owners.push(parameter_id);
        }
    }
    Ok(owners)
}

fn write_channel_group(
    model: &mut Value,
    values: &BTreeMap<u32, [f64; 2]>,
    parameter_id: u32,
    bone_id: u32,
    channels: &[&str],
    components: &[f64; 3],
    axes: [bool; 3],
) -> Result<(), String> {
    let key = closest_keypoint(model, values, parameter_id)?;
    for ((channel, component), write) in channels.iter().zip(components).zip(axes) {
        if write {
            write_channel_key(model, parameter_id, bone_id, channel, key, *component)?;
        }
    }
    Ok(())
}

fn write_channel_key(
    model: &mut Value,
    parameter_id: u32,
    bone_id: u32,
    channel: &str,
    key: [usize; 2],
    value: f64,
) -> Result<(), String> {
    let missing = format!("parameter {parameter_id} binding channel {channel:?} is missing the closest key");
    let parameter = find_parameter_mut(model, parameter_id)
        .ok_or_else(|| format!("parameter {parameter_id} was not found"))?;
    let bindings = parameter
        .get_mut("bindings")
        .and_then(Value::as_array_mut)
        .ok_or("bindings must be an array")?;
    let index = bindings
        .iter()
        .position(|binding| {
            binding.get("node").and_then(Value::as_u64).and_then(valid_id) == Some(bone_id)
                && binding.get("param_name").and_then(Value::as_str) == Some(channel)
        })
        .ok_or_else(|| format!("bone {bone_id} has no single translation parameter"))?;
    let values = bindings[index]
        .get_mut("values")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| missing.clone())?;
    let column = values
        .get_mut(key[0])
        .and_then(Value::as_array_mut)
        .ok_or_else(|| missing.clone())?;
    let cell = column.get_mut(key[1]).ok_or(missing)?;
    *cell = json!(value);
    if let Some(flags) = bindings[index].get_mut("isSet").and_then(Value::as_array_mut) {
        if let Some(column) = flags.get_mut(key[0]).and_then(Value::as_array_mut) {
            if let Some(flag) = column.get_mut(key[1]) {
                *flag = json!(true);
            }
        }
    }
    Ok(())
}

fn parameter_ids(model: &Value) -> Vec<u32> {
    let mut ids = Vec::new();
    if let Some(list) = model.get("param").and_then(Value::as_array) {
        collect_parameter_ids(list, &mut ids);
    }
    ids
}

fn collect_parameter_ids(list: &[Value], ids: &mut Vec<u32>) {
    for item in list {
        if let Some(id) = item.get("uuid").and_then(Value::as_u64).and_then(valid_id) {
            ids.push(id);
        }
        if let Some(children) = item.get("children").and_then(Value::as_array) {
            collect_parameter_ids(children, ids);
        }
    }
}

fn rig_local_origin(model: &Value, root_inverse: Mat4, bone_id: u32) -> Result<[f64; 3], String> {
    let world = transform_point(world_matrix(model, bone_id)?, [0.0, 0.0, 0.0])?;
    let local = transform_point(root_inverse, world)?;
    if !local.iter().all(|value| value.is_finite()) {
        return Err("non-finite matrix".into());
    }
    Ok(local)
}

fn point3(point: [f64; 3]) -> Value {
    json!([point[0], point[1], point[2]])
}

fn effective_pivots(model: &Value, root_id: u32, selected: u32) -> Result<Vec<Value>, String> {
    let bones = build_runtime_limited(model, root_id, None, None, false)?;
    let Some(runtime) = bones.iter().find(|bone| bone.id == selected) else {
        return Ok(Vec::new());
    };
    // A target that cannot supply vertices is omitted from the scale. D still
    // draws the bone geometry and only skips that pivot.
    let world_scale = depth_world_scale_inner(model, model, root_id, false)?;
    let root =
        nodes::find_node(model, root_id).ok_or_else(|| format!("node {root_id} was not found"))?;
    let Some(bindings) = root.get("bindings").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    let mut pivots = Vec::new();
    for binding in bindings {
        let sources = binding
            .get("sourceBoneUuids")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !sources.iter().any(|id| id.as_u64() == Some(u64::from(selected))) {
            continue;
        }
        let Some(target_id) = binding
            .get("target")
            .and_then(Value::as_u64)
            .and_then(valid_id)
        else {
            continue;
        };
        let Some(target) = nodes::find_node(model, target_id) else {
            continue;
        };
        if !is_binding_target(target) {
            continue;
        }
        let Some(records) = skip_bad_pivot(source_records(binding, &bones))? else {
            continue;
        };
        let Some(source) = records
            .iter()
            .find(|record| bones[record.bone_index].id == selected)
        else {
            continue;
        };
        let Some(vertices) = skip_bad_pivot(target_vertices(target))? else {
            continue;
        };
        let Some(raw_depths) = skip_bad_pivot(snapshot_depths(target, vertices.len()))? else {
            continue;
        };
        let scaled: Vec<f64> = raw_depths.iter().map(|depth| depth * world_scale).collect();
        let Some(target_to_root) =
            skip_bad_pivot(target_to_root(model, model, root_id, target_id))?
        else {
            continue;
        };
        let Some(shift) = skip_bad_pivot(rotation_pivot_shift(
            &vertices,
            &scaled,
            target_to_root,
            runtime.rest_head,
            source,
            world_scale,
        ))?
        else {
            continue;
        };
        if shift.abs() <= 1e-4 {
            continue;
        }
        let bone_point = transform_point(runtime.no_yaw_skin, runtime.rest_head)?;
        let effective = transform_point(
            runtime.no_yaw_skin,
            [
                runtime.rest_head[0] - shift,
                runtime.rest_head[1],
                runtime.rest_head[2],
            ],
        )?;
        if !bone_point.iter().chain(effective.iter()).all(|value| value.is_finite()) {
            return Err("non-finite matrix".into());
        }
        pivots.push(json!({
            "target": target_id,
            "bonePoint": point3(bone_point),
            "effectivePoint": point3(effective),
            "rotationPivotXShift": shift,
            "rotation": source.rotation,
        }));
    }
    Ok(pivots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn near(actual: [f64; 2], expected: [f64; 2]) -> bool {
        (actual[0] - expected[0]).abs() <= ABSOLUTE_TOLERANCE
            && (actual[1] - expected[1]).abs() <= ABSOLUTE_TOLERANCE
    }

    #[test]
    fn the_closest_keypoint_of_a_repeating_parameter_is_found_on_the_circle() {
        let mut model = json!({"param": [{"uuid": 7, "name": "Wheel", "is_vec2": false, "min": [0.0, 0.0],
            "max": [4.0, 1.0], "defaults": [0.0, 0.0], "axis_points": [[0.0, 0.5, 1.0], [0.0]]}]});
        let key = |model: &Value, x: f64| closest_keypoint(model, &BTreeMap::from([(7, [x, 0.0])]), 7).unwrap();
        // On a line a value past the end stays at the last keypoint.
        assert_eq!(key(&model, 6.0), [2, 0]);
        model["param"][0]["repeat"] = json!(true);
        // Evaluation takes 6 as 2, 4 as 0 and -0.5 as 3.5. The keypoint is the one nearest to that point.
        for (x, index) in [(6.0, 1), (4.0, 0), (-0.5, 2), (3.9, 2), (1.9, 1)] {
            assert_eq!(key(&model, x), [index, 0], "value {x}");
        }
    }

    #[test]
    fn a_value_far_past_the_range_picks_the_keypoint_of_the_value_it_wraps_to() {
        // Over 0..3, 1e17 leaves 1: a third of the way round, nearest to the middle keypoint.
        let model = json!({"param": [{"uuid": 7, "name": "Wheel", "is_vec2": false, "repeat": true, "min": [0.0, 0.0],
            "max": [3.0, 1.0], "defaults": [0.0, 0.0], "axis_points": [[0.0, 0.5, 1.0], [0.0]]}]});
        let key = |x: f64| closest_keypoint(&model, &BTreeMap::from([(7, [x, 0.0])]), 7).unwrap();
        assert_eq!(key(1.0), [1, 0]);
        assert_eq!(key(1e17), [1, 0]);
    }

    #[test]
    fn inmath_quaternion_matches_the_published_vector_case() {
        let quat = Quat {
            w: 3.0,
            x: 0.0,
            y: 1.0,
            z: 2.0,
        };
        let rotated = rotate_vec(quat, [1.0, 2.0, 3.0]);
        assert!((rotated[0] + 2.0).abs() < 1e-6);
        assert!((rotated[1] - 36.0).abs() < 1e-6);
        assert!((rotated[2] - 38.0).abs() < 1e-6);
    }

    #[test]
    fn depth_edit_euler_round_trips_through_the_pose_quaternion() {
        for sample in [
            [0.0, 0.0, 0.4],
            [0.0, 0.0, -1.2],
            [0.2, -0.3, 0.5],
            [0.1, 0.0, 0.0],
            [-0.4, 0.2, 0.7],
            [0.0, -std::f64::consts::FRAC_PI_2, 0.8],
            [0.3, std::f64::consts::FRAC_PI_2, -0.5],
        ] {
            let quat = depth_edit_rotation(sample[0], sample[1], sample[2]);
            let euler = euler_from_depth_edit(quat).unwrap();
            if sample[1].abs() < 1.0 {
                assert!((euler[0] - sample[0]).abs() < 1e-6, "{sample:?} -> {euler:?}");
                assert!((euler[1] - sample[1]).abs() < 1e-6, "{sample:?} -> {euler:?}");
                assert!((euler[2] - sample[2]).abs() < 1e-6, "{sample:?} -> {euler:?}");
            }
            let again = depth_edit_rotation(euler[0], euler[1], euler[2]);
            let start = rotate_vec(quat, [0.2, 1.0, -0.4]);
            let back = rotate_vec(again, [0.2, 1.0, -0.4]);
            assert!(
                distance(start, back) < 1e-6,
                "sample {sample:?} euler {euler:?} start {start:?} back {back:?}"
            );
        }
    }

    #[test]
    fn solve_ik_bends_a_two_bone_chain_toward_a_rig_local_target() {
        let mut session = crate::Session::new();
        let (rig_id, parent, child) = ik_pair(&mut session);
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]})));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":child})));
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let solved = ok(&session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0,80.0,0.0]
        })));
        let before_distance = solved["distanceBefore"].as_f64().unwrap();
        let after_distance = solved["distanceAfter"].as_f64().unwrap();
        assert!(after_distance < before_distance, "{solved}");
        let once = bone_rotation(&mut session, child);
        assert!(once[2].abs() > 1e-3, "{once:?}");
        let again = ok(&session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0,80.0,0.0]
        })));
        assert!(again["distanceAfter"].as_f64().unwrap() <= after_distance + 1e-3, "{again}");
        let twice = bone_rotation(&mut session, child);
        assert!((twice[2] - once[2]).abs() < 1e-3, "{once:?} {twice:?}");
        let mut guard = 0;
        while ok(&session.handle_request(&json!({"op":"snapshot"})))["model"] != before["model"] {
            ok(&session.handle_request(&json!({"op":"undo"})));
            guard += 1;
            assert!(guard <= 3, "undo did not restore the pre-solve model");
        }

        let rest = ok(&session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[0.0,200.0,0.0]
        })));
        assert!(rest["distanceAfter"].as_f64().unwrap() <= rest["distanceBefore"].as_f64().unwrap() + 1e-6);
        assert!(bone_rotation(&mut session, child)[2].abs() < 1e-3);

        ok(&session.handle_request(&json!({
            "op":"rig.set_constraint","bone":parent,"constraint":{"lockRotation":true}
        })));
        let locked = bone_rotation(&mut session, parent);
        let bent = ok(&session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0,80.0,0.0]
        })));
        assert!(bent["distanceAfter"].as_f64().unwrap() < bent["distanceBefore"].as_f64().unwrap());
        assert_eq!(bone_rotation(&mut session, parent), locked);

        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":999,"target":[1.0,0.0,0.0]
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
        let failed = session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[1.0,"no",0.0]
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
        ok(&session.handle_request(&json!({
            "op":"rig.set_constraint","bone":child,"constraint":{"hingeAxis":[0.0,0.0,0.0]}
        })));
        let zero = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0,80.0,0.0]
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), zero);
    }

    #[test]
    fn solve_ik_uses_a_missing_hinge_as_z_and_writes_the_driven_key() {
        let mut session = crate::Session::new();
        let (rig_id, _parent, child) = ik_pair(&mut session);
        let snap = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let mut model = snap["model"].clone();
        assert!(strip_hinge(&mut model["nodes"], child));
        ok(&session.handle_request(&json!({"op":"new","model":model})));
        let open = ok(&session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0,80.0,0.0]
        })));
        assert!(open["distanceAfter"].as_f64().unwrap() < open["distanceBefore"].as_f64().unwrap(), "{open}");

        let mut session = crate::Session::new();
        let (rig_id, parent, child) = ik_pair(&mut session);
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]})));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":child})));
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Bend"})));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        for bone_id in [parent, child] {
            ok(&session.handle_request(&json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],
                "rotation_x":0.0,"rotation_y":0.0,"rotation_degrees":0.0
            })));
            ok(&session.handle_request(&json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[0,0],
                "rotation_x":0.2,"rotation_y":0.0,"rotation_degrees":0.0
            })));
        }
        let other_key = rotation_cell(&mut session, parameter_id, child, 0);
        let other_deform = deform_key_cell(&mut session, parameter_id, grid_id, 0);
        let closest_before = rotation_cell(&mut session, parameter_id, child, 1);
        let solved = ok(&session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0,80.0,0.0]
        })));
        assert!(solved["distanceAfter"].as_f64().unwrap() < solved["distanceBefore"].as_f64().unwrap());
        assert_eq!(rotation_cell(&mut session, parameter_id, child, 0), other_key);
        assert_eq!(deform_key_cell(&mut session, parameter_id, grid_id, 0), other_deform);
        assert_ne!(rotation_cell(&mut session, parameter_id, child, 1), closest_before);
        assert_eq!(bone_rotation(&mut session, child), [0.0, 0.0, 0.0]);

        ok(&session.handle_request(&json!({"op":"undo"})));
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[parent],"key":[1,0],
            "translation":[40.0,0.0]
        })));
        let shifted = ok(&session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[40.0,200.0,0.0]
        })));
        assert!(shifted["distanceAfter"].as_f64().unwrap() < 1.0, "{shifted}");
        let keyed = rotation_cell(&mut session, parameter_id, child, 1);
        assert!(keyed[2].abs() < 1e-2, "keyed translation should keep the chain straight: {keyed:?}");
        ok(&session.handle_request(&json!({"op":"undo"})));
        let extra = ok(&session.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Extra"})));
        let extra_id = extra["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":extra_id,"nodes":[child],"key":[1,0],"rotation_y":0.1
        })));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.solve_ik","root":rig_id,"endEffector":child,"target":[70.0,80.0,0.0]
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn node_euler_round_trips_and_mirroring_a_matrix_is_an_involution() {
        for sample in [[0.2, -0.15, 0.4], [0.05, 0.1, -0.2], [0.0, 0.0, 0.5]] {
            let quat = euler_rotation(sample[0], sample[1], sample[2]);
            let euler = euler_from_node_rotation(quat).unwrap();
            for (actual, expected) in euler.iter().zip(sample.iter()) {
                assert!((actual - expected).abs() < 1e-6, "{sample:?} -> {euler:?}");
            }
            let matrix = quat_matrix(quat);
            let restored = quat_from_matrix(matrix).unwrap();
            let start = rotate_vec(quat, [0.2, 1.0, -0.4]);
            let back = rotate_vec(restored, [0.2, 1.0, -0.4]);
            assert!(
                distance(start, back) < 1e-6,
                "{sample:?} {start:?} {back:?}"
            );
        }
        let local = mul_mat(
            translation_matrix([20.0, 30.0, 4.0]),
            quat_matrix(euler_rotation(0.05, 0.1, -0.2)),
        );
        let (translation, rotation) = decompose_trs(local, [1.0, 1.0, 1.0]).unwrap();
        assert!((translation[0] - 20.0).abs() < 1e-6);
        assert!((translation[1] - 30.0).abs() < 1e-6);
        assert!((translation[2] - 4.0).abs() < 1e-6);
        assert!((rotation[0] - 0.05).abs() < 1e-6);
        assert!((rotation[1] - 0.1).abs() < 1e-6);
        assert!((rotation[2] + 0.2).abs() < 1e-6);
        let twice = mirror_x_matrix(mirror_x_matrix(local));
        assert!(matrices_near(twice, local), "{twice:?}");
    }

    #[test]
    fn mirror_reflects_a_child_under_a_rotated_parent() {
        let mut session = crate::Session::new();
        let rig = ok(&session
            .handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = create_bone(
            &mut session,
            rig_id,
            "Parent",
            [10.0, 0.0, 0.0],
            [10.0, 100.0, 0.0],
            0.15,
        );
        ok(&session.handle_request(&json!({
            "op":"node.transform","id":parent,"rotation":[0.2,-0.15,0.4]
        })));
        let child = create_bone(
            &mut session,
            parent,
            "Child",
            [30.0, 30.0, 4.0],
            [30.0, 130.0, 4.0],
            0.35,
        );
        ok(&session.handle_request(&json!({
            "op":"node.transform","id":child,"rotation":[0.05,0.1,-0.2]
        })));
        let grid = ok(&session
            .handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":child}),
        ));
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let parent_before = bone_node(&mut session, parent);
        let child_before = bone_node(&mut session, child);
        let child_world_before =
            rig_space_world(&before["model"], rig_id as u32, child as u32).unwrap();
        let failed = session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[parent],"axis":"x"
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(
            failed["message"]
                .as_str()
                .unwrap()
                .contains("closed subtree"),
            "{failed}"
        );
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"snapshot"}))),
            before
        );

        ok(&session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[child],"axis":"x"
        })));
        let mirrored = bone_node(&mut session, child);
        assert_eq!(
            bone_node(&mut session, parent)["transform"],
            parent_before["transform"]
        );
        assert_eq!(
            bone_node(&mut session, parent)["restHead"],
            parent_before["restHead"]
        );
        assert_vec3(
            &mirrored["restHead"],
            mirror_point(vec3(&child_before["restHead"])),
        );
        assert_vec3(
            &mirrored["restTail"],
            mirror_point(vec3(&child_before["restTail"])),
        );
        assert!((mirrored["restRoll"].as_f64().unwrap() + 0.35).abs() < 1e-12);
        assert!(mirrored.get("vertexWeights").is_none());
        let rig_node = ok(&session.handle_request(&json!({"op":"node.get","id":rig_id})));
        assert!(rig_node["bindings"][0].get("vertexWeights").is_none());
        let naive = [
            -child_before["transform"]["trans"][0].as_f64().unwrap(),
            child_before["transform"]["trans"][1].as_f64().unwrap(),
            child_before["transform"]["trans"][2].as_f64().unwrap(),
        ];
        let actual = vec3(&mirrored["transform"]["trans"]);
        assert!(
            distance(actual, naive) > 1e-2,
            "pose must follow the reflected world, not a negated local translation: {actual:?} {naive:?}"
        );
        let mid = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let child_world = rig_space_world(&mid["model"], rig_id as u32, child as u32).unwrap();
        assert!(
            matrices_near(child_world, mirror_x_matrix(child_world_before)),
            "child world {child_world:?}\nexpected {:?}",
            mirror_x_matrix(child_world_before)
        );

        ok(&session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[child],"axis":"x"
        })));
        let restored = bone_node(&mut session, child);
        assert_vec3(&restored["restHead"], vec3(&child_before["restHead"]));
        assert_vec3(&restored["restTail"], vec3(&child_before["restTail"]));
        assert!((restored["restRoll"].as_f64().unwrap() - 0.35).abs() < 1e-12);
        assert_vec3_tol(
            &restored["transform"]["trans"],
            vec3(&child_before["transform"]["trans"]),
            1e-5,
        );
        assert_vec3_tol(
            &restored["transform"]["rot"],
            vec3(&child_before["transform"]["rot"]),
            1e-5,
        );
        let twice = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let world_twice = rig_space_world(&twice["model"], rig_id as u32, child as u32).unwrap();
        assert!(matrices_near(world_twice, child_world_before));

        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"snapshot"})))["model"],
            mid["model"]
        );
        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"snapshot"})))["model"],
            before["model"]
        );
    }

    #[test]
    fn mirror_writes_reflected_channels_and_fails_closed() {
        let mut session = crate::Session::new();
        let rig = ok(&session
            .handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = create_bone(
            &mut session,
            rig_id,
            "Parent",
            [10.0, 0.0, 0.0],
            [10.0, 80.0, 0.0],
            0.0,
        );
        ok(&session.handle_request(&json!({
            "op":"node.transform","id":parent,"rotation":[0.0,0.0,0.6]
        })));
        let child = create_bone(
            &mut session,
            parent,
            "Child",
            [24.0, 16.0, 2.0],
            [24.0, 70.0, 2.0],
            0.2,
        );
        let grid = ok(&session
            .handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[-1.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":child}),
        ));
        let parameter =
            ok(&session
                .handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Pose"})));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        for key in [0, 1] {
            ok(&session.handle_request(&json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[child],"key":[key,0],
                "translation":[0.0,0.0],"translation_z":0.0,
                "rotation_x":0.0,"rotation_y":0.0,"rotation_degrees":0.0
            })));
        }
        let base = bone_node(&mut session, child);
        let base_trans = vec3(&base["transform"]["trans"]);
        let base_rot = vec3(&base["transform"]["rot"]);
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let world_before = rig_space_world(&before["model"], rig_id as u32, child as u32).unwrap();
        let other_before = rotation_cell(&mut session, parameter_id, child, 0);
        ok(&session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[child],"axis":"x"
        })));
        let after_base = bone_node(&mut session, child);
        assert_eq!(after_base["transform"]["trans"], base["transform"]["trans"]);
        assert_eq!(after_base["transform"]["rot"], base["transform"]["rot"]);
        assert_eq!(
            rotation_cell(&mut session, parameter_id, child, 0),
            other_before
        );
        let keyed_trans = translation_cell(&mut session, parameter_id, child, 1);
        let keyed_rot = rotation_cell(&mut session, parameter_id, child, 1);
        let mut posed = before["model"].clone();
        restore_transform_vec(
            &mut posed,
            child as u32,
            "trans",
            [
                base_trans[0] + keyed_trans[0],
                base_trans[1] + keyed_trans[1],
                base_trans[2] + keyed_trans[2],
            ],
        )
        .unwrap();
        restore_transform_vec(
            &mut posed,
            child as u32,
            "rot",
            [
                base_rot[0] + keyed_rot[0],
                base_rot[1] + keyed_rot[1],
                base_rot[2] + keyed_rot[2],
            ],
        )
        .unwrap();
        let world_after = rig_space_world(&posed, rig_id as u32, child as u32).unwrap();
        assert!(
            matrices_near(world_after, mirror_x_matrix(world_before)),
            "driven channels should reconstruct the reflected world\n{world_after:?}\n{:?}",
            mirror_x_matrix(world_before)
        );
        let closest = deform_key_cell(&mut session, parameter_id, grid_id, 1);
        let other = deform_key_cell(&mut session, parameter_id, grid_id, 0);
        assert_ne!(
            closest, other,
            "the driven flush should read the reflected channels"
        );
        let mirrored = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"snapshot"})))["model"],
            before["model"]
        );

        let rejected = [
            json!({"op":"rig.mirror","root":rig_id,"bones":[child],"axis":"y"}),
            json!({"op":"rig.mirror","root":rig_id,"bones":[999],"axis":"x"}),
            json!({"op":"rig.mirror","root":rig_id,"bones":[],"axis":"x"}),
        ];
        for request in rejected {
            let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
            let failed = session.handle_request(&request);
            assert_eq!(failed["succeeded"], false, "{request} {failed}");
            assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
        }
        let snap = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let mut broken = snap["model"].clone();
        assert!(write_rest_component(
            &mut broken["nodes"],
            child,
            "restHead",
            json!("no")
        ));
        ok(&session.handle_request(&json!({"op":"new","model":broken})));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[child],"axis":"x"
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(
            failed["message"]
                .as_str()
                .unwrap()
                .contains("non-finite rest"),
            "{failed}"
        );
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);

        ok(&session.handle_request(&json!({"op":"new","model":mirrored["model"].clone()})));
        let extra = ok(&session
            .handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Split"})));
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":extra["uuid"].as_u64().unwrap(),"nodes":[child],
            "key":[1,0],"translation_z":0.0
        })));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[child],"axis":"x"
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(
            failed["message"]
                .as_str()
                .unwrap()
                .contains("no single translation parameter"),
            "{failed}"
        );
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn mirror_permutes_weights_only_for_a_fully_selected_binding() {
        let mut session = crate::Session::new();
        let rig = ok(&session
            .handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let left = create_bone(
            &mut session,
            rig_id,
            "Left",
            [-20.0, 0.0, 0.0],
            [-20.0, 40.0, 0.0],
            0.0,
        );
        let right = create_bone(
            &mut session,
            rig_id,
            "Right",
            [20.0, 0.0, 0.0],
            [20.0, 40.0, 0.0],
            0.0,
        );
        let grid = ok(&session
            .handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[-1.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":left}),
        ));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":right}),
        ));
        let plain = ok(&session.handle_request(
            &json!({"op":"node.create","className":"GridDeformer","name":"Plain"}),
        ));
        let plain_id = plain["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[plain_id],"axisX":[-1.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":plain_id,"bone":left}),
        ));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":plain_id,"bone":right}),
        ));
        let weights = json!([
            {"bone": left, "values": [1.0, 0.0, 0.2, 0.8]},
            {"bone": right, "values": [0.0, 1.0, 0.8, 0.2]}
        ]);
        ok(&session.handle_request(&json!({
            "op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,"weights":weights
        })));
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[left],"axis":"x"
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(
            failed["message"]
                .as_str()
                .unwrap()
                .contains("not all selected"),
            "{failed}"
        );
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"snapshot"}))),
            before
        );

        ok(&session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[left,right],"axis":"x"
        })));
        let rig_node = ok(&session.handle_request(&json!({"op":"node.get","id":rig_id})));
        let weighted = binding_of(&rig_node, grid_id);
        let unweighted = binding_of(&rig_node, plain_id);
        assert!(unweighted.get("vertexWeights").is_none());
        assert_eq!(
            weighted["vertexWeights"],
            json!([[0.0, 1.0, 0.8, 0.2], [1.0, 0.0, 0.2, 0.8]])
        );
        let mid = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[left,right],"axis":"x"
        })));
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"snapshot"})))["model"],
            before["model"]
        );
        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"snapshot"})))["model"],
            mid["model"]
        );
        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"snapshot"})))["model"],
            before["model"]
        );

        let odd = ok(&session
            .handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Odd"})));
        let odd_id = odd["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[odd_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":odd_id,"bone":left}),
        ));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":odd_id,"bone":right}),
        ));
        ok(&session.handle_request(&json!({
            "op":"rig.set_vertex_weights","root":rig_id,"target":odd_id,"weights":[
                {"bone": left, "values": [1.0, 0.0, 1.0, 0.0]},
                {"bone": right, "values": [0.0, 1.0, 0.0, 1.0]}
            ]
        })));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[left,right],"axis":"x"
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(
            failed["message"].as_str().unwrap().contains("unpaired"),
            "{failed}"
        );
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn mirror_pairs_weights_in_rig_space_not_target_local_xy() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let left = create_bone(
            &mut session,
            rig_id,
            "Left",
            [-20.0, 0.0, 0.0],
            [-20.0, 40.0, 0.0],
            0.0,
        );
        let right = create_bone(
            &mut session,
            rig_id,
            "Right",
            [20.0, 0.0, 0.0],
            [20.0, 40.0, 0.0],
            0.0,
        );
        let grid = ok(&session.handle_request(
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        ));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[-1.0,1.0],"axisY":[0.0,1.0]
        })));
        ok(&session.handle_request(&json!({
            "op":"node.transform","id":grid_id,
            "translation":[8.0,3.0,1.0],"rotation":[0.1,-0.2,0.4]
        })));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":left}),
        ));
        ok(&session.handle_request(
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":right}),
        ));
        ok(&session.handle_request(&json!({
            "op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,"weights":[
                {"bone": left, "values": [1.0, 0.0, 0.2, 0.8]},
                {"bone": right, "values": [0.0, 1.0, 0.8, 0.2]}
            ]
        })));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[left,right],"axis":"x"
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(
            failed["message"].as_str().unwrap().contains("unpaired"),
            "{failed}"
        );
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
        let rig_node = ok(&session.handle_request(&json!({"op":"node.get","id":rig_id})));
        assert_eq!(
            binding_of(&rig_node, grid_id)["vertexWeights"],
            json!([[1.0, 0.0, 0.2, 0.8], [0.0, 1.0, 0.8, 0.2]])
        );
    }

    #[test]
    fn mirror_rejects_a_sheared_local_under_a_scaled_rotated_parent() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        ));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = create_bone(
            &mut session,
            rig_id,
            "Parent",
            [0.0, 0.0, 0.0],
            [0.0, 80.0, 0.0],
            0.0,
        );
        ok(&session.handle_request(&json!({
            "op":"node.transform","id":parent,"rotation":[0.0,0.0,0.5],"scale":[2.0,0.5]
        })));
        let child = create_bone(
            &mut session,
            parent,
            "Child",
            [10.0, 20.0, 0.0],
            [10.0, 70.0, 0.0],
            0.0,
        );
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let revision = held["revision"].as_u64().unwrap();
        let failed = session.handle_request(&json!({
            "op":"rig.mirror","root":rig_id,"bones":[child],"axis":"x"
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(
            failed["message"]
                .as_str()
                .unwrap()
                .contains("not representable"),
            "{failed}"
        );
        let after = ok(&session.handle_request(&json!({"op":"snapshot"})));
        assert_eq!(after["model"], held["model"]);
        assert_eq!(after["revision"].as_u64().unwrap(), revision);
    }

    fn create_bone(
        session: &mut crate::Session,
        parent: u64,
        name: &str,
        head: [f64; 3],
        tail: [f64; 3],
        roll: f64,
    ) -> u64 {
        let created = ok(&session.handle_request(&json!({
            "op":"node.create","className":"DepthBone","name":name,"parents":[parent],
            "restHead":head,"restTail":tail,"restRoll":roll
        })));
        created["created"][0]["uuid"].as_u64().unwrap()
    }

    fn bone_node(session: &mut crate::Session, id: u64) -> Value {
        ok(&session.handle_request(&json!({"op":"node.get","id":id})))
    }

    fn vec3(value: &Value) -> [f64; 3] {
        [
            value[0].as_f64().unwrap(),
            value[1].as_f64().unwrap(),
            value[2].as_f64().unwrap(),
        ]
    }

    fn assert_vec3(actual: &Value, expected: [f64; 3]) {
        let actual = vec3(actual);
        assert!(distance(actual, expected) < 1e-9, "{actual:?} {expected:?}");
    }

    fn assert_vec3_tol(actual: &Value, expected: [f64; 3], tolerance: f64) {
        let actual = vec3(actual);
        assert!(
            distance(actual, expected) <= tolerance,
            "{actual:?} {expected:?}"
        );
    }

    fn matrices_near(actual: Mat4, expected: Mat4) -> bool {
        actual
            .iter()
            .flatten()
            .zip(expected.iter().flatten())
            .all(|(left, right)| (left - right).abs() <= 1e-5)
    }

    fn rig_space_world(model: &Value, root_id: u32, bone_id: u32) -> Result<Mat4, String> {
        let root = world_matrix(model, root_id)?;
        Ok(mul_mat(invert(root)?, world_matrix(model, bone_id)?))
    }

    fn translation_cell(
        session: &mut crate::Session,
        parameter: u64,
        bone_id: u64,
        key: usize,
    ) -> [f64; 3] {
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.get","id":parameter})));
        let mut value = [0.0; 3];
        for (index, name) in ["transform.t.x", "transform.t.y", "transform.t.z"]
            .iter()
            .enumerate()
        {
            let binding = parameter["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|binding| binding["param_name"] == *name && binding["node"] == bone_id)
                .unwrap();
            value[index] = binding["values"][key][0].as_f64().unwrap();
        }
        value
    }

    fn binding_of(root: &Value, target: u64) -> Value {
        root["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["target"] == target)
            .unwrap()
            .clone()
    }

    fn write_rest_component(node: &mut Value, id: u64, key: &str, value: Value) -> bool {
        if node.get("uuid").and_then(Value::as_u64) == Some(id) {
            node.as_object_mut().unwrap().insert(key.to_string(), value);
            return true;
        }
        if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
            for child in children {
                if write_rest_component(child, id, key, value.clone()) {
                    return true;
                }
            }
        }
        false
    }


    fn ik_pair(session: &mut crate::Session) -> (u64, u64, u64) {
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = bone(session, rig_id, "Parent", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let child = bone(session, parent, "Child", [0.0, 100.0, 0.0], [0.0, 200.0, 0.0]);
        (rig_id, parent, child)
    }

    fn bone_rotation(session: &mut crate::Session, id: u64) -> [f64; 3] {
        let node = ok(&session.handle_request(&json!({"op":"node.get","id":id})));
        let rot = node["transform"]["rot"].as_array().unwrap();
        [
            rot[0].as_f64().unwrap(),
            rot[1].as_f64().unwrap(),
            rot[2].as_f64().unwrap(),
        ]
    }

    fn rotation_cell(session: &mut crate::Session, parameter: u64, bone_id: u64, key: usize) -> [f64; 3] {
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.get","id":parameter})));
        let mut euler = [0.0; 3];
        for (index, name) in ["transform.r.x", "transform.r.y", "transform.r.z"].iter().enumerate() {
            let binding = parameter["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|binding| binding["param_name"] == *name && binding["node"] == bone_id)
                .unwrap();
            euler[index] = binding["values"][key][0].as_f64().unwrap();
        }
        euler
    }

    fn deform_key_cell(session: &mut crate::Session, parameter: u64, node: u64, key: usize) -> Value {
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.get","id":parameter})));
        parameter["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == node)
            .unwrap()["values"][key][0]
            .clone()
    }

    fn strip_hinge(node: &mut Value, id: u64) -> bool {
        if node.get("uuid").and_then(Value::as_u64) == Some(id) {
            node.as_object_mut().unwrap().remove("hingeAxis");
            return true;
        }
        if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
            for child in children {
                if strip_hinge(child, id) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn yaw_of_a_unit_grid_matches_a_y_rotation() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[rig_id],"boneId":"Spine",
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        );
        call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[rig_id],"boneId":"Extra",
                "restHead":[40.0,0.0,0.0],"restTail":[40.0,80.0,0.0]
            }),
        );
        let grid = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        );
        let grid_id = grid["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        let spine = find_bone(&mut session, "Spine");
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":spine}),
        );
        let parameter = call(
            &mut session,
            &json!({"op":"parameter.create","dimensions":1,"name":"Yaw"}),
        );
        let parameter_id = parameter["result"]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[spine],"key":[1,0],"rotation_y":std::f64::consts::FRAC_PI_2
            }),
        );
        let preview = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"parameter":parameter_id}),
        );
        assert_eq!(preview["succeeded"], true);
        let before = call(&mut session, &json!({"op":"snapshot"}));
        let again = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"parameter":parameter_id}),
        );
        let after = call(&mut session, &json!({"op":"snapshot"}));
        assert_eq!(before["result"]["canUndo"], after["result"]["canUndo"]);
        assert_eq!(before["result"]["modified"], after["result"]["modified"]);
        assert_eq!(before["result"]["revision"], after["result"]["revision"]);
        assert_eq!(before["result"]["model"], after["result"]["model"]);
        let offsets = offset_pairs(&again["result"]["targets"][0]["offsets"]);
        assert!(near(offsets[0], [0.0, 0.0]));
        assert!(near(offsets[1], [-1.0, 0.0]), "{:?}", offsets[1]);
        assert!(near(offsets[2], [0.0, 0.0]));
        assert!(near(offsets[3], [-1.0, 0.0]));

        call(
            &mut session,
            &json!({"op":"depth.set","target":grid_id,"depths":[0.0,0.0,10.0,10.0]}),
        );
        let deep = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"parameter":parameter_id}),
        );
        let deep_offsets = offset_pairs(&deep["result"]["targets"][0]["offsets"]);
        assert!(near(deep_offsets[2], [-10.0, 0.0]), "{:?}", deep_offsets[2]);
        assert!(near(deep_offsets[3], [-11.0, 0.0]), "{:?}", deep_offsets[3]);

        let path = call(
            &mut session,
            &json!({"op":"node.create","className":"PathDeformer","name":"Path"}),
        );
        let path_id = path["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"deformer.define_vertices","nodes":[path_id],"vertices":[0.0,0.0,1.0,0.0,0.0,1.0,1.0,1.0]}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":path_id,"bone":spine}),
        );
        let path_preview = call(
            &mut session,
            &json!({
                "op":"rig.preview_deform","root":rig_id,"targets":[path_id],"parameter":parameter_id
            }),
        );
        let path_offsets = offset_pairs(&path_preview["result"]["targets"][0]["offsets"]);
        assert!(near(path_offsets[1], [-1.0, 0.0]), "{:?}", path_offsets[1]);

        let mut path_model = session.snapshot()["model"].clone();
        nodes::find_node_mut(&mut path_model, path_id as u32).unwrap()["depths"] =
            json!([0.0, 0.0, 10.0, 10.0]);
        let mut path_session = crate::Session::new();
        call(&mut path_session, &json!({"op":"new", "model":path_model}));
        call(&mut path_session, &json!({"op":"value.set", "id":parameter_id, "value":[1.0,0.0]}));
        let loaded_preview = call(&mut path_session, &json!({
            "op":"rig.preview_deform", "root":rig_id, "targets":[path_id], "parameter":parameter_id
        }));
        let loaded_offsets = offset_pairs(&loaded_preview["result"]["targets"][0]["offsets"]);
        assert!(near(loaded_offsets[2], [-10.0, 0.0]), "{:?}", loaded_offsets[2]);
        assert!(near(loaded_offsets[3], [-11.0, 0.0]), "{:?}", loaded_offsets[3]);

        call(
            &mut session,
            &json!({"op":"depth.set","target":grid_id,"depths":[5.0,5.0,5.0,5.0]}),
        );
        call(
            &mut session,
            &json!({
                "op":"rig.set_source_settings","root":rig_id,"target":grid_id,"bone":spine,"settings":{"rotation":0.0}
            }),
        );
        let plain = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"targets":[grid_id],"parameter":parameter_id}),
        );
        call(
            &mut session,
            &json!({
                "op":"rig.set_source_settings","root":rig_id,"target":grid_id,"bone":spine,"settings":{"rotation":0.2}
            }),
        );
        let rotated = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"targets":[grid_id],"parameter":parameter_id}),
        );
        let plain_offsets = offset_pairs(&plain["result"]["targets"][0]["offsets"]);
        let rotated_offsets = offset_pairs(&rotated["result"]["targets"][0]["offsets"]);
        assert!(rotated_offsets
            .iter()
            .zip(plain_offsets.iter())
            .any(|(a, b)| !near(*a, *b)));

        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[spine],"key":[1,0],"rotation_y":0.0
            }),
        );
        let ignored = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"targets":[grid_id],"parameter":parameter_id}),
        );
        for offset in offset_pairs(&ignored["result"]["targets"][0]["offsets"]) {
            assert!(near(offset, [0.0, 0.0]), "{offset:?}");
        }
    }

    #[test]
    fn allow_parent_false_keeps_the_child_at_rest() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        let parent = call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[rig_id],"boneId":"Parent",
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        );
        let parent_id = parent["result"]["created"][0]["uuid"].as_u64().unwrap();
        let child = call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[parent_id],"boneId":"Child",
                "restHead":[0.0,100.0,0.0],"restTail":[0.0,200.0,0.0]
            }),
        );
        let child_id = child["result"]["created"][0]["uuid"].as_u64().unwrap();
        let grid = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        );
        let grid_id = grid["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":child_id}),
        );
        let parameter = call(
            &mut session,
            &json!({"op":"parameter.create","dimensions":1,"name":"Yaw"}),
        );
        let parameter_id = parameter["result"]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[parent_id],"key":[1,0],"rotation_y":std::f64::consts::FRAC_PI_2
            }),
        );
        let snapshot = call(&mut session, &json!({"op":"snapshot"}));
        let mut model = snapshot["result"]["model"].clone();
        set_allow(&mut model, child_id, false);
        let values = BTreeMap::from([(parameter_id as u32, [0.0, 0.0])]);
        let isolated = run(
            &model,
            &values,
            rig_id as u32,
            &[grid_id as u32],
            Some(parameter_id as u32),
            false,
            &BTreeMap::new(),
        )
        .unwrap();
        for offset in offset_pairs(&isolated.body["targets"][0]["offsets"]) {
            assert!(near(offset, [0.0, 0.0]), "{offset:?}");
        }
        set_allow(&mut model, child_id, true);
        let inherited = run(
            &model,
            &values,
            rig_id as u32,
            &[grid_id as u32],
            Some(parameter_id as u32),
            false,
            &BTreeMap::new(),
        )
        .unwrap();
        let inherited_offsets = offset_pairs(&inherited.body["targets"][0]["offsets"]);
        assert!(inherited_offsets
            .iter()
            .any(|offset| !near(*offset, [0.0, 0.0])));
    }

    #[test]
    fn apply_writes_one_key_and_failures_leave_the_model() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        let bone = call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[rig_id],"boneId":"Spine",
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        );
        let bone_id = bone["result"]["created"][0]["uuid"].as_u64().unwrap();
        let grid = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        );
        let grid_id = grid["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        );
        let held = call(&mut session, &json!({"op":"snapshot"}));
        let missing = call(
            &mut session,
            &json!({"op":"rig.apply_deform","root":rig_id}),
        );
        assert_eq!(missing["succeeded"], false);
        assert_eq!(missing["message"], "No armed parameter");
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"]["model"],
            held["result"]["model"]
        );
        let parameter = call(
            &mut session,
            &json!({"op":"parameter.create","dimensions":1,"name":"Yaw"}),
        );
        let parameter_id = parameter["result"]["uuid"].as_u64().unwrap();
        let undriven = call(
            &mut session,
            &json!({"op":"rig.apply_deform","root":rig_id,"parameter":parameter_id}),
        );
        assert_eq!(undriven["succeeded"], false);
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],"rotation_y":0.3
            }),
        );
        let applied = call(
            &mut session,
            &json!({"op":"rig.apply_deform","root":rig_id,"parameter":parameter_id}),
        );
        assert_eq!(applied["succeeded"], true);
        let cell = deform_cell(&mut session, parameter_id, grid_id);
        assert_eq!(cell.as_array().unwrap().len(), 4);
        call(&mut session, &json!({"op":"undo"}));
        let undone = call(
            &mut session,
            &json!({"op":"parameter.get","id":parameter_id}),
        );
        let bindings = undone["result"]["bindings"].as_array().unwrap();
        assert!(bindings
            .iter()
            .all(|binding| binding["param_name"] != "deform"));
        let parent = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Parent"}),
        );
        let parent_id = parent["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"node.move","nodes":[grid_id],"parent":parent_id}),
        );
        call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[parent_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":parent_id,"bone":bone_id}),
        );
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],"rotation_y":std::f64::consts::FRAC_PI_2
            }),
        );
        let nested = call(
            &mut session,
            &json!({"op":"rig.apply_deform","root":rig_id,"parameter":parameter_id}),
        );
        assert_eq!(nested["succeeded"], true, "{nested}");
        let child = offsets_for(&nested["result"]["targets"], grid_id);
        for offset in child {
            assert!(near(offset, [0.0, 0.0]), "{offset:?}");
        }
        let parent_offsets = offsets_for(&nested["result"]["targets"], parent_id);
        assert!(
            near(parent_offsets[1], [-1.0, 0.0]),
            "{:?}",
            parent_offsets[1]
        );
    }

    #[test]
    fn generated_sparse_deforms_fill_1d_and_2d_cells_and_keep_history() {
        for dimensions in [1, 2] {
            let mut session = crate::Session::new();
            let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot"})));
            let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
            let bone_id = bone(&mut session, rig_id, "Bone", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
            let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer"})));
            let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
            ok(&session.handle_request(&json!({
                "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]
            })));
            ok(&session.handle_request(&json!({
                "op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id
            })));
            let parameter = ok(&session.handle_request(&json!({
                "op":"parameter.create","dimensions":dimensions,"name":"Sparse"
            })));
            let parameter_id = parameter["uuid"].as_u64().unwrap();
            let (min, max, axis_y, y_count) = if dimensions == 1 {
                ([-1.0, 0.0], [1.0, 1.0], vec![0.0], 1)
            } else {
                ([-1.0, -1.0], [1.0, 1.0], vec![0.0, 1.0], 2)
            };
            ok(&session.handle_request(&json!({
                "op":"parameter.set_axes","id":parameter_id,"min":min,"max":max,
                "axis_points":[[0.0,1.0],axis_y]
            })));
            let physics = ok(&session.handle_request(&json!({
                "op":"node.create","className":"SimplePhysics"
            })));
            let physics_id = physics["created"][0]["uuid"].as_u64().unwrap();
            ok(&session.handle_request(&json!({
                "op":"physics.set","node":physics_id,"param":parameter_id
            })));

            let snapshot = ok(&session.handle_request(&json!({"op":"snapshot"})));
            let mut model = snapshot["model"].clone();
            let values = (0..2)
                .map(|_| {
                    (0..y_count)
                        .map(|_| Value::Array(Vec::new()))
                        .collect::<Vec<_>>()
                })
                .collect::<Vec<_>>();
            let flags = (0..2)
                .map(|_| vec![Value::Bool(false); y_count])
                .collect::<Vec<_>>();
            let parameter = model["param"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|parameter| parameter["uuid"].as_u64() == Some(parameter_id))
                .unwrap();
            parameter["bindings"] = json!([{
                "node":grid_id,
                "param_name":"deform",
                "interpolate_mode":"Linear",
                "values":values,
                "isSet":flags,
                "extension":{"keep":true}
            }]);
            ok(&session.handle_request(&json!({"op":"new","model":model})));
            let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
            ok(&session.handle_request(&json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],
                "key":[1,0],"rotation_y":0.5
            })));
            let after = ok(&session.handle_request(&json!({"op":"snapshot"})));
            let parameter = ok(&session.handle_request(&json!({"op":"parameter.get","id":parameter_id})));
            let binding = parameter["bindings"]
                .as_array()
                .unwrap()
                .iter()
                .find(|binding| binding["param_name"] == "deform" && binding["node"] == grid_id)
                .unwrap();
            let expected_flags = if dimensions == 1 {
                json!([[false], [true]])
            } else {
                json!([[false, false], [true, false]])
            };
            assert_eq!(binding["isSet"], expected_flags);
            assert_eq!(binding["extension"], json!({"keep":true}));
            for x in 0..2 {
                for y in 0..y_count {
                    assert_eq!(binding["values"][x][y].as_array().unwrap().len(), 4);
                }
            }

            ok(&session.handle_request(&json!({"op":"undo"})));
            let undone = ok(&session.handle_request(&json!({"op":"snapshot"})));
            assert_eq!(undone["model"], before["model"]);
            ok(&session.handle_request(&json!({"op":"redo"})));
            let redone = ok(&session.handle_request(&json!({"op":"snapshot"})));
            assert_eq!(redone["model"], after["model"]);
            ok(&session.handle_request(&json!({"op":"animation.create","name":"Sparse"})));
            ok(&session.handle_request(&json!({
                "op":"value.set","id":parameter_id,"value":[0.0,0.0]
            })));
            ok(&session.handle_request(&json!({"op":"animation.seek","name":"Sparse","frame":0})));
            let step = session.handle_request(&json!({"op":"physics.step","dt":0.01}));
            assert_eq!(step["succeeded"], true, "{step}");
            let evaluated = ok(&session.handle_request(&json!({"op":"node.get","id":grid_id})));
            assert_eq!(evaluated["deformation"].as_array().unwrap().len(), 4);
        }
    }

    #[test]
    fn target_translation_scale_and_x_rotation_change_the_offset() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        let bone = call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[rig_id],"boneId":"Spine",
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        );
        let bone_id = bone["result"]["created"][0]["uuid"].as_u64().unwrap();
        let grid = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid"}),
        );
        let grid_id = grid["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        );
        let parameter = call(
            &mut session,
            &json!({"op":"parameter.create","dimensions":1,"name":"Yaw"}),
        );
        let parameter_id = parameter["result"]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],
                "rotation_y":std::f64::consts::FRAC_PI_2
            }),
        );
        call(
            &mut session,
            &json!({"op":"node.transform","id":grid_id,"translation":[10.0,0.0,0.0]}),
        );
        let moved = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"parameter":parameter_id}),
        );
        let moved_offsets = offset_pairs(&moved["result"]["targets"][0]["offsets"]);
        assert!(
            near(moved_offsets[0], [-10.0, 0.0]),
            "{:?}",
            moved_offsets[0]
        );
        assert!(
            near(moved_offsets[1], [-11.0, 0.0]),
            "{:?}",
            moved_offsets[1]
        );
        call(
            &mut session,
            &json!({"op":"node.transform","id":grid_id,"scale":[2.0,2.0]}),
        );
        let scaled = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"parameter":parameter_id}),
        );
        let scaled_offsets = offset_pairs(&scaled["result"]["targets"][0]["offsets"]);
        assert!(
            near(scaled_offsets[0], [-5.0, 0.0]),
            "{:?}",
            scaled_offsets[0]
        );
        call(
            &mut session,
            &json!({"op":"node.transform","id":grid_id,"translation":[0.0,0.0,0.0],"scale":[1.0,1.0],"rotation":[std::f64::consts::FRAC_PI_2,0.0,0.0]}),
        );
        let rotated = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"parameter":parameter_id}),
        );
        let rotated_offsets = offset_pairs(&rotated["result"]["targets"][0]["offsets"]);
        assert!(
            near(rotated_offsets[2], [-1.0, -1.0]),
            "{:?}",
            rotated_offsets[2]
        );
    }

    #[test]
    fn mesh_group_stopper_does_not_subtract_origin_influence() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        let bone = call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[rig_id],"boneId":"Spine",
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        );
        let bone_id = bone["result"]["created"][0]["uuid"].as_u64().unwrap();
        let parent = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Parent"}),
        );
        let parent_id = parent["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[parent_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        let group = call(
            &mut session,
            &json!({"op":"node.create","className":"MeshGroup","name":"Group","parents":[parent_id]}),
        );
        let group_id = group["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"node.transform","id":group_id,"translation":[1.0,0.0,0.0]}),
        );
        let grid = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Child","parents":[group_id]}),
        );
        let grid_id = grid["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        );
        let parameter = call(
            &mut session,
            &json!({"op":"parameter.create","dimensions":1,"name":"Yaw"}),
        );
        let parameter_id = parameter["result"]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],
                "rotation_y":std::f64::consts::FRAC_PI_2
            }),
        );
        let alone = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"targets":[grid_id],"parameter":parameter_id}),
        );
        let alone_offsets = offset_pairs(&alone["result"]["targets"][0]["offsets"]);
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":parent_id,"bone":bone_id}),
        );
        let parent_applied = call(
            &mut session,
            &json!({"op":"rig.apply_deform","root":rig_id,"targets":[parent_id],"parameter":parameter_id}),
        );
        let parent_offsets = offset_pairs(&parent_applied["result"]["targets"][0]["offsets"]);
        assert!(
            near(parent_offsets[1], [-1.0, 0.0]),
            "{:?}",
            parent_offsets[1]
        );
        let with_ancestor = call(
            &mut session,
            &json!({"op":"rig.preview_deform","root":rig_id,"targets":[grid_id],"parameter":parameter_id}),
        );
        let with_offsets = offset_pairs(&with_ancestor["result"]["targets"][0]["offsets"]);
        assert_eq!(with_offsets.len(), alone_offsets.len());
        for (left, right) in with_offsets.iter().zip(alone_offsets.iter()) {
            assert!(near(*left, *right), "{left:?} {right:?}");
        }
    }

    #[test]
    fn ancestor_types_follow_soul2d_live_must_propagate() {
        let mask = propagation_case("Mask", [0.0, 0.0, 0.0], None, None);
        assert!(
            mask.with_child.iter().all(|offset| near(*offset, [0.0, 0.0])),
            "mask should pass the parent grid through: {:?}",
            mask.with_child
        );
        assert!(mask.alone.iter().any(|offset| !near(*offset, [0.0, 0.0])));

        let composite = propagation_case("Composite", [0.0, 0.0, 0.0], None, None);
        assert!(
            composite
                .with_child
                .iter()
                .all(|offset| near(*offset, [0.0, 0.0])),
            "a missing propagate_meshgroup stays true: {:?}",
            composite.with_child
        );

        let stopped = propagation_case(
            "Composite",
            [0.0, 0.0, 0.0],
            Some(("propagate_meshgroup", false)),
            None,
        );
        assert_offsets_near(&stopped.with_child, &stopped.alone);
        assert!(stopped.alone.iter().any(|offset| !near(*offset, [0.0, 0.0])));

        let dynamic = propagation_case(
            "DynamicComposite",
            [0.0, 0.0, 0.0],
            Some(("propagate_meshgroup", true)),
            None,
        );
        assert_offsets_near(&dynamic.with_child, &dynamic.alone);
        assert!(dynamic.with_child.iter().any(|offset| !near(*offset, [0.0, 0.0])));

        let camera = propagation_case("Camera", [1.0, 0.0, 0.0], None, None);
        assert!(
            near(camera.parent[1], [-1.0, 0.0]),
            "parent corner offset: {:?}",
            camera.parent[1]
        );
        for (with, alone) in camera.with_child.iter().zip(camera.alone.iter()) {
            assert!(
                near(*with, [alone[0] - camera.parent[1][0], alone[1] - camera.parent[1][1]]),
                "camera origin influence {with:?} {alone:?} {:?}",
                camera.parent[1]
            );
        }
        let frozen = propagation_case("Camera", [1.0, 0.0, 0.0], None, Some(false));
        assert_offsets_near(&frozen.with_child, &frozen.alone);

        for kind in ["SimplePhysics", "Shapes"] {
            let mut session = crate::Session::new();
            let built = build_propagation(&mut session, "Camera", [1.0, 0.0, 0.0], None, None);
            call(
                &mut session,
                &json!({"op":"rig.add_source","root":built.rig,"target":built.parent,"bone":built.bone}),
            );
            let snap = call(&mut session, &json!({"op":"snapshot"}));
            let mut model = snap["result"]["model"].clone();
            let middle = find_named(&model, "Middle");
            set_type(&mut model, middle, kind);
            let loaded = call(&mut session, &json!({"op":"new","model":model}));
            assert_eq!(loaded["succeeded"], true, "{loaded}");
            let before = call(&mut session, &json!({"op":"snapshot"}));
            let preview = preview_target(&mut session, built.rig, built.parameter, built.child);
            assert_offsets_near(&preview, &camera.with_child);
            let after = call(&mut session, &json!({"op":"snapshot"}));
            assert_eq!(after["result"]["model"], before["result"]["model"], "{kind}");
            assert_eq!(after["result"]["revision"], before["result"]["revision"], "{kind}");
            assert_eq!(after["result"]["canUndo"], before["result"]["canUndo"], "{kind}");
        }

        let mut session = crate::Session::new();
        let built = build_propagation(&mut session, "Camera", [1.0, 0.0, 0.0], None, None);
        let before = call(&mut session, &json!({"op":"snapshot"}));
        let revision = before["result"]["revision"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":built.rig,"target":built.parent,"bone":built.bone}),
        );
        let edited = call(&mut session, &json!({"op":"snapshot"}));
        assert_eq!(edited["result"]["revision"].as_u64().unwrap(), revision + 1);
        assert_ne!(edited["result"]["model"], before["result"]["model"]);
        call(&mut session, &json!({"op":"undo"}));
        let undone = call(&mut session, &json!({"op":"snapshot"}));
        assert_eq!(undone["result"]["model"], before["result"]["model"]);
        assert_eq!(undone["result"]["revision"].as_u64().unwrap(), revision + 2);
        call(&mut session, &json!({"op":"redo"}));
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"]["model"],
            edited["result"]["model"]
        );
    }

    #[test]
    fn unknown_propagation_fails_closed() {
        let mut session = crate::Session::new();
        let rig = call(
            &mut session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        let bone = call(
            &mut session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[rig_id],"boneId":"Spine",
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        );
        let bone_id = bone["result"]["created"][0]["uuid"].as_u64().unwrap();
        let parent = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Parent"}),
        );
        let parent_id = parent["result"]["created"][0]["uuid"].as_u64().unwrap();
        let middle = call(
            &mut session,
            &json!({"op":"node.create","className":"Node","name":"Middle","parents":[parent_id]}),
        );
        let middle_id = middle["result"]["created"][0]["uuid"].as_u64().unwrap();
        let grid = call(
            &mut session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Grid","parents":[middle_id]}),
        );
        let grid_id = grid["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":parent_id,"bone":bone_id}),
        );
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":bone_id}),
        );
        let parameter = call(
            &mut session,
            &json!({"op":"parameter.create","dimensions":1,"name":"Yaw"}),
        );
        let parameter_id = parameter["result"]["uuid"].as_u64().unwrap();
        call(
            &mut session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],"rotation_y":0.2
            }),
        );
        let snapshot = call(&mut session, &json!({"op":"snapshot"}));
        let mut model = snapshot["result"]["model"].clone();
        set_type(&mut model, middle_id, "Widget");
        let values = BTreeMap::from([(parameter_id as u32, [0.0, 0.0])]);
        let failed = run(
            &model,
            &values,
            rig_id as u32,
            &[],
            Some(parameter_id as u32),
            true,
            &BTreeMap::new(),
        );
        match failed {
            Err(message) => assert!(
                message.contains("cannot match D ancestor grid propagation"),
                "{message}"
            ),
            Ok(_) => panic!("unknown propagation must fail closed"),
        }
        let loaded = call(&mut session, &json!({"op":"new","model": model}));
        assert_eq!(loaded["succeeded"], true, "{loaded}");
        let held = call(&mut session, &json!({"op":"snapshot"}));
        let apply = call(
            &mut session,
            &json!({"op":"rig.apply_deform","root":rig_id,"parameter":parameter_id}),
        );
        assert_eq!(apply["succeeded"], false, "{apply}");
        assert!(
            apply["message"]
                .as_str()
                .unwrap()
                .contains("cannot match D ancestor grid propagation"),
            "{apply}"
        );
        assert_eq!(call(&mut session, &json!({"op":"snapshot"})), held);
    }

    #[test]
    fn simple_physics_settings_do_not_change_propagation() {
        let camera = propagation_case("Camera", [1.0, 0.0, 0.0], None, None);
        let mut session = crate::Session::new();
        let built = build_propagation(&mut session, "SimplePhysics", [1.0, 0.0, 0.0], None, None);
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":built.rig,"target":built.parent,"bone":built.bone}),
        );
        let before_preview = call(&mut session, &json!({"op":"snapshot"}));
        let offsets = preview_target(&mut session, built.rig, built.parameter, built.child);
        assert_offsets_near(&offsets, &camera.with_child);
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"]["model"],
            before_preview["result"]["model"]
        );
        let middle = find_named(&before_preview["result"]["model"], "Middle");
        let updated = call(
            &mut session,
            &json!({
                "op":"physics.set","node":middle,
                "param": built.parameter,
                "model_type":"SpringPendulum",
                "map_mode":"YX",
                "gravity":9.0,
                "length":1.0,
                "frequency":30.0,
                "angle_damping":0.0,
                "length_damping":0.0,
                "output_scale":[4.0, 4.0],
                "local_only": true
            }),
        );
        assert_eq!(updated["succeeded"], true, "{updated}");
        let after = call(&mut session, &json!({"op":"snapshot"}));
        assert_ne!(after["result"]["model"], before_preview["result"]["model"]);
        assert_eq!(after["result"]["model"]["param"], before_preview["result"]["model"]["param"]);
        let changed = preview_target(&mut session, built.rig, built.parameter, built.child);
        assert_offsets_near(&changed, &offsets);
        assert_eq!(
            call(&mut session, &json!({"op":"snapshot"}))["result"]["model"],
            after["result"]["model"]
        );
    }

    fn call(session: &mut crate::Session, request: &Value) -> Value {
        session.handle_request(request)
    }

    fn find_bone(session: &mut crate::Session, bone_id: &str) -> u64 {
        fn walk(node: &Value, bone_id: &str) -> Option<u64> {
            if node.get("boneId").and_then(Value::as_str) == Some(bone_id) {
                return node.get("uuid").and_then(Value::as_u64);
            }
            node.get("children")
                .and_then(Value::as_array)
                .and_then(|children| children.iter().find_map(|child| walk(child, bone_id)))
        }
        let snapshot = session.handle_request(&json!({"op":"snapshot"}));
        walk(&snapshot["result"]["model"]["nodes"], bone_id).unwrap()
    }

    fn offsets_for(targets: &Value, id: u64) -> Vec<[f64; 2]> {
        let target = targets
            .as_array()
            .unwrap()
            .iter()
            .find(|target| target["target"].as_u64() == Some(id))
            .unwrap();
        offset_pairs(&target["offsets"])
    }

    struct Propagation {
        alone: Vec<[f64; 2]>,
        with_child: Vec<[f64; 2]>,
        parent: Vec<[f64; 2]>,
    }

    struct Built {
        rig: u64,
        bone: u64,
        parent: u64,
        parameter: u64,
        child: u64,
    }

    fn propagation_case(
        class_name: &str,
        translation: [f64; 3],
        intermediate_flag: Option<(&str, bool)>,
        translate_children: Option<bool>,
    ) -> Propagation {
        let mut session = crate::Session::new();
        let built = build_propagation(
            &mut session,
            class_name,
            translation,
            intermediate_flag,
            translate_children,
        );
        let alone = preview_target(&mut session, built.rig, built.parameter, built.child);
        call(
            &mut session,
            &json!({"op":"rig.add_source","root":built.rig,"target":built.parent,"bone":built.bone}),
        );
        Propagation {
            alone,
            with_child: preview_target(&mut session, built.rig, built.parameter, built.child),
            parent: preview_target(&mut session, built.rig, built.parameter, built.parent),
        }
    }

    fn build_propagation(
        session: &mut crate::Session,
        class_name: &str,
        translation: [f64; 3],
        intermediate_flag: Option<(&str, bool)>,
        translate_children: Option<bool>,
    ) -> Built {
        let rig = call(
            session,
            &json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"}),
        );
        let rig_id = rig["result"]["created"][0]["uuid"].as_u64().unwrap();
        let bone = call(
            session,
            &json!({
                "op":"node.create","className":"DepthBone","parents":[rig_id],"boneId":"Spine",
                "restHead":[0.0,0.0,0.0],"restTail":[0.0,100.0,0.0]
            }),
        );
        let bone_id = bone["result"]["created"][0]["uuid"].as_u64().unwrap();
        let parent = call(
            session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Parent"}),
        );
        let parent_id = parent["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            session,
            &json!({"op":"deformer.define_grid","nodes":[parent_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        let middle = call(
            session,
            &json!({"op":"node.create","className":class_name,"name":"Middle","parents":[parent_id]}),
        );
        assert_eq!(middle["succeeded"], true, "{middle}");
        let middle_id = middle["result"]["created"][0]["uuid"].as_u64().unwrap();
        if translation != [0.0, 0.0, 0.0] {
            call(
                session,
                &json!({"op":"node.transform","id":middle_id,"translation":translation}),
            );
        }
        // Mask cannot take children through node.create. A loaded tree still
        // can, which is the shape `gridPropagationStopper` walks.
        let child = call(
            session,
            &json!({"op":"node.create","className":"GridDeformer","name":"Child","parents":[parent_id]}),
        );
        assert_eq!(child["succeeded"], true, "{child}");
        let child_id = child["result"]["created"][0]["uuid"].as_u64().unwrap();
        call(
            session,
            &json!({"op":"deformer.define_grid","nodes":[child_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]}),
        );
        let mut model = call(session, &json!({"op":"snapshot"}))["result"]["model"].clone();
        reparent(&mut model, child_id, middle_id);
        if let Some((key, value)) = intermediate_flag {
            set_bool(&mut model, middle_id, key, value);
        }
        if let Some(value) = translate_children {
            set_bool(&mut model, parent_id, "translateChildren", value);
        }
        let reloaded = call(session, &json!({"op":"new","model": model}));
        assert_eq!(reloaded["succeeded"], true, "{reloaded}");
        call(
            session,
            &json!({"op":"rig.add_source","root":rig_id,"target":child_id,"bone":bone_id}),
        );
        let parameter = call(
            session,
            &json!({"op":"parameter.create","dimensions":1,"name":"Yaw"}),
        );
        let parameter_id = parameter["result"]["uuid"].as_u64().unwrap();
        call(
            session,
            &json!({
                "op":"binding.set_trs","parameter":parameter_id,"nodes":[bone_id],"key":[1,0],
                "rotation_y":std::f64::consts::FRAC_PI_2
            }),
        );
        Built {
            rig: rig_id,
            bone: bone_id,
            parent: parent_id,
            parameter: parameter_id,
            child: child_id,
        }
    }

    fn preview_target(
        session: &mut crate::Session,
        rig: u64,
        parameter: u64,
        target: u64,
    ) -> Vec<[f64; 2]> {
        let preview = call(
            session,
            &json!({"op":"rig.preview_deform","root":rig,"parameter":parameter,"targets":[target]}),
        );
        assert_eq!(preview["succeeded"], true, "{preview}");
        offset_pairs(&preview["result"]["targets"][0]["offsets"])
    }

    fn assert_offsets_near(left: &[[f64; 2]], right: &[[f64; 2]]) {
        assert_eq!(left.len(), right.len());
        for (lhs, rhs) in left.iter().zip(right.iter()) {
            assert!(near(*lhs, *rhs), "{lhs:?} {rhs:?}");
        }
    }

    fn reparent(model: &mut Value, child_id: u64, parent_id: u64) {
        fn take(node: &mut Value, child_id: u64) -> Option<Value> {
            let children = node.get_mut("children")?.as_array_mut()?;
            let index = children
                .iter()
                .position(|child| child.get("uuid").and_then(Value::as_u64) == Some(child_id))?;
            Some(children.remove(index))
        }
        fn detach(node: &mut Value, child_id: u64) -> Option<Value> {
            if let Some(child) = take(node, child_id) {
                return Some(child);
            }
            let children = node.get_mut("children")?.as_array_mut()?;
            for child in children {
                if let Some(found) = detach(child, child_id) {
                    return Some(found);
                }
            }
            None
        }
        fn attach(node: &mut Value, parent_id: u64, child: Value) -> bool {
            if node.get("uuid").and_then(Value::as_u64) == Some(parent_id) {
                node.get_mut("children")
                    .and_then(Value::as_array_mut)
                    .unwrap()
                    .push(child);
                return true;
            }
            if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
                for node in children {
                    if attach(node, parent_id, child.clone()) {
                        return true;
                    }
                }
            }
            false
        }
        let child = detach(&mut model["nodes"], child_id).unwrap();
        assert!(attach(&mut model["nodes"], parent_id, child));
    }

    fn find_named(model: &Value, name: &str) -> u64 {
        fn walk(node: &Value, name: &str) -> Option<u64> {
            if node.get("name").and_then(Value::as_str) == Some(name) {
                return node.get("uuid").and_then(Value::as_u64);
            }
            node.get("children")
                .and_then(Value::as_array)
                .and_then(|children| children.iter().find_map(|child| walk(child, name)))
        }
        walk(&model["nodes"], name).unwrap()
    }

    fn set_bool(model: &mut Value, id: u64, key: &str, value: bool) {
        fn walk(node: &mut Value, id: u64, key: &str, value: bool) -> bool {
            if node.get("uuid").and_then(Value::as_u64) == Some(id) {
                node.as_object_mut()
                    .unwrap()
                    .insert(key.to_string(), json!(value));
                return true;
            }
            if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
                for child in children {
                    if walk(child, id, key, value) {
                        return true;
                    }
                }
            }
            false
        }
        assert!(walk(&mut model["nodes"], id, key, value));
    }

    fn set_type(model: &mut Value, id: u64, kind: &str) {
        fn walk(node: &mut Value, id: u64, kind: &str) -> bool {
            if node.get("uuid").and_then(Value::as_u64) == Some(id) {
                node.as_object_mut()
                    .unwrap()
                    .insert("type".into(), json!(kind));
                return true;
            }
            if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
                for child in children {
                    if walk(child, id, kind) {
                        return true;
                    }
                }
            }
            false
        }
        walk(&mut model["nodes"], id, kind);
    }

    fn offset_pairs(value: &Value) -> Vec<[f64; 2]> {
        value
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                let pair = pair.as_array().unwrap();
                [pair[0].as_f64().unwrap(), pair[1].as_f64().unwrap()]
            })
            .collect()
    }

    fn set_allow(model: &mut Value, id: u64, value: bool) {
        fn walk(node: &mut Value, id: u64, value: bool) -> bool {
            if node.get("uuid").and_then(Value::as_u64) == Some(id) {
                node.as_object_mut()
                    .unwrap()
                    .insert("allowParentToTargets".into(), json!(value));
                return true;
            }
            if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
                for child in children {
                    if walk(child, id, value) {
                        return true;
                    }
                }
            }
            false
        }
        walk(&mut model["nodes"], id, value);
    }

    #[test]
    fn overlay_classifies_links_and_does_not_write_history() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = bone(&mut session, rig_id, "Parent", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let selected = bone(&mut session, parent, "Selected", [0.0, 10.0, 0.0], [0.0, 20.0, 0.0]);
        let grandchild = bone(&mut session, selected, "Grand", [0.0, 10.0, 0.0], [0.0, 10.0, 0.0]);
        let sibling = bone(&mut session, parent, "Sibling", [10.0, 0.0, 0.0], [10.0, 10.0, 0.0]);
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]})));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":selected})));
        ok(&session.handle_request(&json!({
            "op":"rig.set_source_settings","root":rig_id,"target":grid_id,"bone":selected,
            "settings":{"rotation":0.5,"depthOffset":1.0}
        })));
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let overlay = ok(&session.handle_request(&json!({"op":"rig.overlay","root":rig_id,"selected":selected})));
        assert_eq!(overlay["selectedPoints"].as_array().unwrap().len(), 1);
        assert!((overlay["selectedPoints"][0][1].as_f64().unwrap() - 10.0).abs() < 1e-4);
        assert_eq!(overlay["points"].as_array().unwrap().len(), 3);
        assert_eq!(overlay["parentLines"].as_array().unwrap().len(), 1);
        assert_eq!(overlay["childLines"].as_array().unwrap().len(), 1);
        assert_eq!(overlay["lines"].as_array().unwrap().len(), 1);
        assert!(overlay["effectivePivots"].as_array().unwrap().len() >= 1);
        assert!(overlay["effectivePivots"][0]["rotationPivotXShift"].as_f64().unwrap().abs() > 1e-4);
        let after = ok(&session.handle_request(&json!({"op":"snapshot"})));
        assert_eq!(after, before);
        let other = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Other"})));
        let other_id = other["created"][0]["uuid"].as_u64().unwrap();
        let foreign = bone(&mut session, other_id, "Foreign", [0.0, 0.0, 0.0], [0.0, 10.0, 0.0]);
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({"op":"rig.overlay","root":rig_id,"selected":foreign}));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("not in this rig"));
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
        let _ = (grandchild, sibling);
    }

    #[test]
    fn overlay_follows_the_runtime_pose_of_a_bone_and_its_ancestor() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = bone(&mut session, rig_id, "Parent", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let selected = bone(&mut session, parent, "Selected", [0.0, 10.0, 0.0], [0.0, 20.0, 0.0]);
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]})));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":selected})));
        ok(&session.handle_request(&json!({
            "op":"rig.set_source_settings","root":rig_id,"target":grid_id,"bone":selected,
            "settings":{"rotation":0.5,"depthOffset":1.0}
        })));
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Pose"})));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[selected],"key":[2,0],"translation":[0.0,25.0]
        })));
        let rest = ok(&session.handle_request(&json!({"op":"rig.overlay","root":rig_id,"selected":selected})));
        assert!((rest["selectedPoints"][0][1].as_f64().unwrap() - 10.0).abs() < 1e-4);
        let rest_pivot = rest["effectivePivots"][0]["bonePoint"][1].as_f64().unwrap();
        let stored = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({"op":"value.set","id":parameter_id,"value":[1.0,0.0]})));
        let posed = ok(&session.handle_request(&json!({"op":"rig.overlay","root":rig_id,"selected":selected})));
        assert!((posed["selectedPoints"][0][1].as_f64().unwrap() - 35.0).abs() < 1e-4);
        assert!((posed["effectivePivots"][0]["bonePoint"][1].as_f64().unwrap() - rest_pivot - 25.0).abs() < 1e-4);
        let after = ok(&session.handle_request(&json!({"op":"snapshot"})));
        assert_eq!(after["model"], stored["model"]);
        assert_eq!(after["revision"], stored["revision"]);
        assert_eq!(after["canUndo"], stored["canUndo"]);

        ok(&session.handle_request(&json!({"op":"value.set","id":parameter_id,"value":[0.0,0.0]})));
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[parent],"key":[2,0],"translation":[0.0,40.0]
        })));
        ok(&session.handle_request(&json!({"op":"value.set","id":parameter_id,"value":[1.0,0.0]})));
        let ancestor = ok(&session.handle_request(&json!({"op":"rig.overlay","root":rig_id,"selected":selected})));
        assert!((ancestor["selectedPoints"][0][1].as_f64().unwrap() - 75.0).abs() < 1e-4, "{ancestor}");
        let parent_point = ancestor["points"]
            .as_array()
            .unwrap()
            .iter()
            .find(|point| (point[1].as_f64().unwrap() - 40.0).abs() < 1e-4);
        assert!(parent_point.is_some(), "{ancestor}");
    }

    #[test]
    fn overlay_ignores_the_bone_cap_and_cpu_deform_still_commits() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let mut last = rig_id;
        for index in 0..65 {
            last = bone(
                &mut session,
                rig_id,
                &format!("Bone{index}"),
                [index as f64, 0.0, 0.0],
                [index as f64, 10.0, 0.0],
            );
        }
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let overlay = ok(&session.handle_request(&json!({"op":"rig.overlay","root":rig_id,"selected":last})));
        assert_eq!(overlay["selectedPoints"].as_array().unwrap().len(), 1);
        assert_eq!(overlay["points"].as_array().unwrap().len(), 64);
        assert!(overlay["effectivePivots"].as_array().unwrap().is_empty());
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), before);
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]})));
        let parameter = ok(&session.handle_request(&json!({
            "op":"parameter.create","dimensions":1,"name":"Bend"
        })));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[last],"key":[1,0],
            "rotation_degrees":0.0
        })));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({
            "op":"rig.add_source","root":rig_id,"target":grid_id,"bone":last
        })));
        let saved = deform_values_for(&mut session, parameter_id, grid_id);
        assert!(saved.as_array().unwrap().iter().any(|column| {
            column.as_array().unwrap().iter().any(|cell| cell.is_array())
        }), "{saved}");
        let report = ok(&session.handle_request(&json!({
            "op":"rig.gpu_offsets","root":rig_id,"parameter":parameter_id
        })));
        assert_eq!(report["matched"], false, "{report}");
        assert!(
            report["reason"].as_str().unwrap_or("").contains("maximum bone count"),
            "{report}"
        );
        assert_eq!(deform_values_for(&mut session, parameter_id, grid_id), saved);
        assert_ne!(ok(&session.handle_request(&json!({"op":"snapshot"})))["model"], held["model"]);
    }

    fn deform_values_for(session: &mut crate::Session, parameter: u64, node: u64) -> Value {
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.get","id":parameter})));
        parameter["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == node)
            .unwrap()["values"]
            .clone()
    }

    #[test]
    fn overlay_keeps_bone_geometry_when_one_pivot_cannot_be_built() {
        let transform = || json!({"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]});
        let model = json!({
            "nodes": {
                "uuid": 1, "type": "Node", "transform": transform(),
                "children": [{
                    "uuid": 2, "type": "DepthRigRoot", "transform": transform(),
                    "bindings": [
                        {
                            "target": 10,
                            "sourceBoneUuids": [4],
                            "sourceSettings": [{"bone": 4, "rotation": 0.5, "depthOffset": 1.0, "depthScale": 1.0, "weight": 1.0}]
                        },
                        {
                            "target": 11,
                            "sourceBoneUuids": [4],
                            "sourceSettings": [{"bone": 4, "rotation": 0.5, "depthOffset": 1.0}]
                        },
                        {
                            "target": 12,
                            "sourceBoneUuids": [4],
                            "sourceSettings": [{"bone": 4, "rotation": 0.5, "depthOffset": 1.0}],
                            "influenceRule": {"maxInfluences": "no"}
                        }
                    ],
                    "children": [
                        {
                            "uuid": 3, "type": "DepthBone", "name": "Parent",
                            "restHead": [0.0, 0.0, 0.0], "restTail": [0.0, 100.0, 0.0],
                            "transform": transform(),
                            "children": [{
                                "uuid": 4, "type": "DepthBone", "name": "Selected",
                                "restHead": [0.0, 10.0, 0.0], "restTail": [0.0, 20.0, 0.0],
                                "transform": {"trans": [0.0, 10.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]}
                            }]
                        },
                        {
                            "uuid": 10, "type": "GridDeformer", "transform": transform(),
                            "grid_axis_x": [0.0, 1.0], "grid_axis_y": [0.0, 1.0]
                        },
                        {
                            "uuid": 11, "type": "GridDeformer", "transform": transform(),
                            "grid_axis_x": [0.0], "grid_axis_y": [0.0]
                        },
                        {
                            "uuid": 12, "type": "GridDeformer", "transform": transform(),
                            "grid_axis_x": [0.0, 1.0], "grid_axis_y": [0.0, 1.0]
                        }
                    ]
                }]
            }
        });
        let overlay = overlay_geometry(&model, 2, Some(4)).unwrap();
        assert_eq!(overlay["selectedPoints"].as_array().unwrap().len(), 1);
        assert!((overlay["selectedPoints"][0][1].as_f64().unwrap() - 10.0).abs() < 1e-4);
        assert_eq!(overlay["parentLines"].as_array().unwrap().len(), 1);
        let pivots = overlay["effectivePivots"].as_array().unwrap();
        assert_eq!(pivots.len(), 1, "{overlay}");
        assert_eq!(pivots[0]["target"], 10);
        assert!(pivots[0]["rotationPivotXShift"].as_f64().unwrap().abs() > 1e-4);
    }

    #[test]
    fn persisted_vertex_weights_replace_the_gaussian_score() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let moved = bone(&mut session, rig_id, "Moved", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let still = bone(&mut session, rig_id, "Still", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0],"axisY":[0.0,1.0]})));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":moved})));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":still})));
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Slide"})));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[moved],"key":[1,0],"translation":[10.0,0.0]
        })));
        let gaussian = deform_preview(&mut session, rig_id, parameter_id);
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({
            "op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,
            "weights":[
                {"bone":moved,"values":[1.0,1.0,0.0,0.0]},
                {"bone":still,"values":[0.0,0.0,1.0,1.0]}
            ]
        })));
        let weighted = deform_preview(&mut session, rig_id, parameter_id);
        assert!(near(weighted[0], [10.0, 0.0]), "{weighted:?}");
        assert!(near(weighted[1], [10.0, 0.0]), "{weighted:?}");
        assert!(near(weighted[2], [0.0, 0.0]), "{weighted:?}");
        assert!(near(weighted[3], [0.0, 0.0]), "{weighted:?}");
        assert_ne!(weighted, gaussian);
        assert_eq!(
            stored_weights(&mut session, rig_id, grid_id),
            json!([[1.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 1.0]])
        );
        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"})))["model"], before["model"]);
        ok(&session.handle_request(&json!({"op":"redo"})));
        let foreign = bone(&mut session, rig_id, "Foreign", [5.0, 0.0, 0.0], [5.0, 10.0, 0.0]);
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let rejected = [
            json!({"op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,"weights":[
                {"bone":moved,"values":[1.0,0.0]},
                {"bone":still,"values":[0.0,1.0,0.0,0.0]}
            ]}),
            json!({"op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,"weights":[
                {"bone":moved,"values":[0.2,0.2,0.2,0.2]},
                {"bone":still,"values":[0.2,0.2,0.2,0.2]}
            ]}),
            json!({"op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,"weights":[
                {"bone":moved,"values":[0.0,0.0,0.0,0.0]},
                {"bone":still,"values":[0.0,0.0,0.0,0.0]}
            ]}),
            json!({"op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,"weights":[
                {"bone":foreign,"values":[1.0,1.0,1.0,1.0]}
            ]}),
            json!({"op":"rig.set_vertex_weights","root":rig_id,"target":grid_id,"weights":"no"}),
            json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":foreign}),
            json!({"op":"rig.remove_source","root":rig_id,"target":grid_id,"bone":still}),
            json!({"op":"rig.set_source_settings","root":rig_id,"target":grid_id,"bone":foreign,"settings":{"weight":1.0}}),
            json!({"op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,1.0,2.0],"axisY":[0.0,1.0]}),
            json!({"op":"node.delete","nodes":[still]}),
        ];
        for request in &rejected {
            let failed = session.handle_request(request);
            assert_eq!(failed["succeeded"], false, "{request} -> {failed}");
            assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
        }
        let path = std::env::temp_dir().join(format!(
            "vsoul-vertex-weights-{}-{}.inx",
            std::process::id(),
            rig_id
        ));
        let _ = std::fs::remove_file(&path);
        ok(&session.handle_request(&json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true})));
        let mut reloaded = crate::Session::new();
        ok(&reloaded.handle_request(&json!({"op":"open","path":path.to_string_lossy()})));
        let _ = std::fs::remove_file(&path);
        assert_eq!(
            stored_weights(&mut reloaded, rig_id, grid_id),
            json!([[1.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 1.0]])
        );
        ok(&session.handle_request(&json!({
            "op":"rig.set_source_settings","root":rig_id,"target":grid_id,"bone":still,
            "settings":{"depthOffset":0.25}
        })));
        assert_eq!(
            stored_weights(&mut session, rig_id, grid_id),
            json!([[1.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 1.0]])
        );
        ok(&session.handle_request(&json!({"op":"undo"})));
        ok(&session.handle_request(&json!({"op":"rig.clear_vertex_weights","root":rig_id,"target":grid_id})));
        assert_eq!(deform_preview(&mut session, rig_id, parameter_id), gaussian);
        assert!(stored_weights(&mut session, rig_id, grid_id).is_null());
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":foreign})));
        assert_ne!(deform_preview(&mut session, rig_id, parameter_id), weighted);
    }

    /// A Part with a quad mesh whose first corner is at the node origin, as a node of a model.
    fn quad_part(id: u32, name: &str, size: f64) -> Value {
        json!({
            "uuid": id, "name": name, "type": "Part", "enabled": true, "zsort": 0.0,
            "transform": {"trans": [0.0, 0.0, 0.0], "rot": [0.0, 0.0, 0.0], "scale": [1.0, 1.0]},
            "lockToRoot": false, "pinToMesh": false, "children": [],
            "mesh": {"verts": [0.0, 0.0, size, 0.0, 0.0, size, size, size], "uvs": [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0],
                "indices": [0, 1, 2, 2, 1, 3], "origin": [0.0, 0.0]},
            "textures": [u32::MAX, u32::MAX, u32::MAX], "blend_mode": "Normal", "tint": [1.0, 1.0, 1.0],
            "screenTint": [0.0, 0.0, 0.0], "opacity": 1.0, "emissionStrength": 1.0, "mask_threshold": 0.5,
        })
    }

    #[test]
    fn a_part_is_bound_by_its_own_vertices_depths_and_weights() {
        let mut built = crate::Session::new();
        let rig = ok(&built.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let moved = bone(&mut built, rig_id, "Moved", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let still = bone(&mut built, rig_id, "Still", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let (part, wide, shifted) = (900, 901, 902);
        let mut model = built.snapshot()["model"].clone();
        let children = model["nodes"]["children"].as_array_mut().unwrap();
        children.push(quad_part(part as u32, "Skin", 1.0));
        children.push(quad_part(wide as u32, "Wide", 100.0));
        let mut offset = quad_part(shifted as u32, "Shifted", 1.0);
        offset["mesh"]["verts"] = json!([3.0, 4.0, 4.0, 4.0, 3.0, 5.0, 4.0, 5.0]);
        offset["mesh"]["origin"] = json!([3.0, 4.0]);
        children.push(offset);
        let mut bare = quad_part(903, "Bare", 1.0);
        bare["mesh"] = json!({"verts": [], "uvs": [], "indices": [], "origin": [0.0, 0.0]});
        children.push(bare);
        let mut session = crate::Session::new();
        ok(&session.handle_request(&json!({"op":"new","model":model})));

        // One source, a quarter turn about Y: a vertex moves by its x and by its depth, as on a grid.
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":part,"bone":moved})));
        let bound = ok(&session.handle_request(&json!({"op":"node.get","id":rig_id})));
        assert_eq!((&bound["bindings"][0]["target"], &bound["bindings"][0]["targetKind"]), (&json!(part), &json!("part")));
        let yaw = ok(&session.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Yaw"})));
        let yaw = yaw["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":yaw,"nodes":[moved],"key":[1,0],"rotation_y":std::f64::consts::FRAC_PI_2
        })));
        let flat = deform_preview(&mut session, rig_id, yaw);
        assert!(near(flat[1], [-1.0, 0.0]) && near(flat[2], [0.0, 0.0]) && near(flat[3], [-1.0, 0.0]), "{flat:?}");
        let short = session.handle_request(&json!({"op":"depth.set","target":part,"depths":[0.0, 0.0, 10.0]}));
        assert_eq!(short["message"], "depths length must match target vertices");
        ok(&session.handle_request(&json!({"op":"depth.set","target":part,"depths":[0.0, 0.0, 10.0, 10.0]})));
        let listed = ok(&session.handle_request(&json!({"op":"depth.list","target":part})));
        assert_eq!((&listed["count"], &listed["depths"]), (&json!(4), &json!([0.0, 0.0, 10.0, 10.0])));
        let deep = deform_preview(&mut session, rig_id, yaw);
        assert!(near(deep[1], [-1.0, 0.0]) && near(deep[2], [-10.0, 0.0]) && near(deep[3], [-11.0, 0.0]), "{deep:?}");

        // A bound Part counts toward the size the rig scales depth by, here 100 * 0.42 / 2.9.
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":wide,"bone":moved})));
        let scaled = ok(&session.handle_request(&json!({
            "op":"rig.preview_deform","root":rig_id,"targets":[part],"parameter":yaw
        })));
        let scaled = offset_pairs(&scaled["targets"][0]["offsets"]);
        assert!(near(scaled[2], [-144.827_586, 0.0]) && near(scaled[3], [-145.827_586, 0.0]), "{scaled:?}");
        ok(&session.handle_request(&json!({"op":"rig.remove_source","root":rig_id,"target":wide,"bone":moved})));

        // The bones move a Part where it is drawn, which is its vertices minus the mesh origin: this one is drawn
        // on the same unit square, and turns the same way. It stays bound.
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":shifted,"bone":moved})));
        let drawn = ok(&session.handle_request(&json!({
            "op":"rig.preview_deform","root":rig_id,"targets":[shifted],"parameter":yaw
        })));
        let drawn = offset_pairs(&drawn["targets"][0]["offsets"]);
        assert!(near(drawn[0], [0.0, 0.0]) && near(drawn[1], [-1.0, 0.0]) && near(drawn[3], [-1.0, 0.0]), "{drawn:?}");

        // Weights for each mesh vertex replace the automatic ones, which two bones at one place share evenly. The
        // overlay shows the pivot of a bound Part as it does for a grid.
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":part,"bone":still})));
        let unweighted = session.snapshot()["model"].clone();
        let automatic = gaussian_vertex_weights(&unweighted, &unweighted, rig_id as u32, part as u32).unwrap();
        assert_eq!(automatic.into_iter().map(|row| row.1).collect::<Vec<_>>(), vec![vec![0.5; 4]; 2]);
        let mut turned = unweighted.clone();
        let table = nodes::find_node_mut(&mut turned, rig_id as u32).unwrap()["bindings"].as_array_mut().unwrap();
        let row = table.iter_mut().find(|binding| binding["target"] == part).unwrap();
        row["sourceSettings"] = json!([{"bone": moved, "rotation": 0.5, "depthOffset": 1.0}]);
        let overlay = overlay_geometry(&turned, rig_id as u32, Some(moved as u32)).unwrap();
        let pivots = overlay["effectivePivots"].as_array().unwrap();
        assert!(pivots.iter().any(|pivot| pivot["target"] == part), "{overlay}");
        let slide = ok(&session.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Slide"})));
        let slide = slide["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":slide,"nodes":[moved],"key":[1,0],"translation":[10.0,0.0]
        })));
        let ragged = session.handle_request(&json!({
            "op":"rig.set_vertex_weights","root":rig_id,"target":part,
            "weights":[{"bone":moved,"values":[1.0,1.0,0.0]},{"bone":still,"values":[0.0,0.0,1.0]}]
        }));
        assert_eq!(ragged["message"], "vertex weight length must match the vertex count", "{ragged}");
        ok(&session.handle_request(&json!({
            "op":"rig.set_vertex_weights","root":rig_id,"target":part,
            "weights":[{"bone":moved,"values":[1.0,1.0,0.0,0.0]},{"bone":still,"values":[0.0,0.0,1.0,1.0]}]
        })));
        let weighted = deform_preview(&mut session, rig_id, slide);
        assert!(near(weighted[0], [10.0, 0.0]) && near(weighted[1], [10.0, 0.0]), "{weighted:?}");
        assert!(near(weighted[2], [0.0, 0.0]) && near(weighted[3], [0.0, 0.0]), "{weighted:?}");

        // Applied, the offsets are the Part's own deform key. A later change of the bone key rewrites it, as it does
        // for a grid.
        ok(&session.handle_request(&json!({"op":"rig.apply_deform","root":rig_id,"parameter":slide})));
        assert_eq!(offset_pairs(&deform_cell(&mut session, slide, part)), weighted);
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":slide,"nodes":[moved],"key":[1,0],"translation":[20.0,0.0]
        })));
        let rewritten = offset_pairs(&deform_cell(&mut session, slide, part));
        assert!(near(rewritten[0], [20.0, 0.0]) && near(rewritten[1], [20.0, 0.0]), "{rewritten:?}");
        assert!(near(rewritten[2], [0.0, 0.0]) && near(rewritten[3], [0.0, 0.0]), "{rewritten:?}");

        // While a parameter drives the rig, the Part takes no manual deform key, of any parameter.
        let other = ok(&session.handle_request(&json!({"op":"parameter.create","dimensions":1,"name":"Other"})));
        let locked = session.handle_request(&json!({
            "op":"binding.set_deform","parameter":other["uuid"],"nodes":[part],"key":[0,0],"values":vec![0.0; 8]
        }));
        assert_eq!(locked["message"], format!("node {part} is driven by a rig and cannot accept a manual deform key"));

        // A new mesh origin moves the drawn Part, here to x 1 to 2, and the rig writes its keys again.
        ok(&session.handle_request(&json!({
            "op":"node.property.set","id":shifted,"property":"origin","value":[2.0, 4.0]
        })));

        // Saved and opened again, where no preview lies over the keys, the scene draws the Part at its mesh plus
        // the keys of both parameters. Each has one key, which holds at every value: vertex 1 is turned to x 0 by
        // Yaw and slides with vertex 0.
        let path = std::env::temp_dir().join(format!("soul2d-part-target-{}-{rig_id}.inx", std::process::id()));
        let _ = std::fs::remove_file(&path);
        ok(&session.handle_request(&json!({"op":"save","path":path.to_string_lossy(),"mark_saved":true})));
        let mut reopened = crate::Session::new();
        ok(&reopened.handle_request(&json!({"op":"open","path":path.to_string_lossy()})));
        let _ = std::fs::remove_file(&path);
        let scene = ok(&reopened.handle_request(&json!({"op":"scene.geometry"})));
        let skin = scene["parts"].as_array().unwrap().iter().find(|drawn| drawn["id"] == part).unwrap();
        let drawn = offset_pairs(&skin["vertices"]);
        assert!(near(drawn[0], [20.0, 0.0]) && near(drawn[1], [20.0, 0.0]), "{drawn:?}");
        assert!(near(drawn[2], [0.0, 1.0]) && near(drawn[3], [1.0, 1.0]), "{drawn:?}");
        // The Part with a mesh origin follows its one bone whole: Yaw turns every vertex to x 0, then it slides.
        let offset = scene["parts"].as_array().unwrap().iter().find(|drawn| drawn["id"] == shifted).unwrap();
        let offset = offset_pairs(&offset["vertices"]);
        assert!(near(offset[0], [20.0, 0.0]) && near(offset[1], [20.0, 0.0]), "{offset:?}");
        assert!(near(offset[2], [20.0, 1.0]) && near(offset[3], [20.0, 1.0]), "{offset:?}");
        assert_eq!(stored_weights(&mut reopened, rig_id, part), json!([[1.0, 1.0, 0.0, 0.0], [0.0, 0.0, 1.0, 1.0]]));
        let kept = ok(&reopened.handle_request(&json!({"op":"depth.list","target":part})));
        assert_eq!(kept["depths"], json!([0.0, 0.0, 10.0, 10.0]));

        // A Part without a mesh is not bound, and another kind of node is still no target.
        let bare = session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":903,"bone":moved}));
        assert_eq!(bare["message"], "target Part has no mesh vertices");
        let root = session.snapshot()["model"]["nodes"]["uuid"].as_u64().unwrap();
        let refused = session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":root,"bone":moved}));
        assert_eq!(refused["message"], "target must be GridDeformer, PathDeformer or Part");
    }

    fn deform_preview(session: &mut crate::Session, rig: u64, parameter: u64) -> Vec<[f64; 2]> {
        let preview = ok(&session.handle_request(&json!({
            "op":"rig.preview_deform","root":rig,"parameter":parameter
        })));
        offset_pairs(&preview["targets"][0]["offsets"])
    }

    fn stored_weights(session: &mut crate::Session, rig: u64, grid: u64) -> Value {
        let node = ok(&session.handle_request(&json!({"op":"node.get","id":rig})));
        node["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["target"] == grid)
            .and_then(|binding| binding.get("vertexWeights"))
            .cloned()
            .unwrap_or(Value::Null)
    }

    fn bone(session: &mut crate::Session, parent: u64, name: &str, head: [f64; 3], tail: [f64; 3]) -> u64 {
        let created = ok(&session.handle_request(&json!({
            "op":"node.create","className":"DepthBone","name":name,"parents":[parent],
            "restHead":head,"restTail":tail
        })));
        let id = created["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"node.transform","id":id,"translation":head
        })));
        id
    }

    fn ok(response: &Value) -> Value {
        assert_eq!(response["succeeded"], true, "{response}");
        response["result"].clone()
    }

    fn deform_cell(session: &mut crate::Session, parameter: u64, node: u64) -> Value {
        let parameter = session.handle_request(&json!({"op":"parameter.get","id":parameter}));
        parameter["result"]["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == "deform" && binding["node"] == node)
            .unwrap()["values"][1][0]
            .clone()
    }

    fn channel_at(session: &mut crate::Session, parameter: u64, bone: u64, name: &str, key: usize) -> f64 {
        let parameter = ok(&session.handle_request(&json!({"op":"parameter.get","id":parameter})));
        let binding = parameter["bindings"]
            .as_array()
            .unwrap()
            .iter()
            .find(|binding| binding["param_name"] == name && binding["node"] == bone)
            .unwrap();
        binding["values"][key][0].as_f64().unwrap()
    }

    #[test]
    fn pick_hits_the_child_and_a_miss_does_not_write() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = bone(&mut session, rig_id, "Parent", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let child = bone(&mut session, parent, "Child", [0.0, 10.0, 0.0], [0.0, 20.0, 0.0]);
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let hit = ok(&session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[0.0,10.0,0.0],"radius":1.0
        })));
        assert_eq!(hit["bone"], child);
        let exact = ok(&session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[0.0,10.0,0.0],"radius":0.0
        })));
        assert_eq!(exact["bone"], child);
        let parent_hit = ok(&session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[0.0,1.0,0.0],"radius":2.0
        })));
        assert_eq!(parent_hit["bone"], parent);
        let miss = ok(&session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[100.0,100.0,0.0],"radius":1.0
        })));
        assert!(miss["bone"].is_null());
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), before);
        let held = before;
        let negative = session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[0.0,10.0,0.0],"radius":-0.1
        }));
        assert_eq!(negative["succeeded"], false, "{negative}");
        assert!(negative["message"].as_str().unwrap().contains("non-negative"), "{negative}");
        let infinite = session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[null,0.0,0.0],"radius":1.0
        }));
        assert_eq!(infinite["succeeded"], false, "{infinite}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn undriven_drag_moves_the_child_and_one_undo_restores_it() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let parent = bone(&mut session, rig_id, "Parent", [0.0, 0.0, 0.0], [0.0, 100.0, 0.0]);
        let child = bone(&mut session, parent, "Child", [0.0, 10.0, 0.0], [0.0, 20.0, 0.0]);
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,10.0],"axisY":[0.0,10.0]
        })));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":child})));
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({
            "op":"rig.drag_bone","root":rig_id,"bone":child,"translation":[0.0,40.0,0.0]
        })));
        let node = ok(&session.handle_request(&json!({"op":"node.get","id":child})));
        assert_eq!(node["transform"]["trans"], json!([0.0, 40.0, 0.0]));
        let picked = ok(&session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[0.0,40.0,0.0],"radius":0.5
        })));
        assert_eq!(picked["bone"], child);
        let moved = ok(&session.handle_request(&json!({"op":"snapshot"})));
        assert_eq!(moved["revision"], before["revision"].as_u64().unwrap() + 1);
        assert_eq!(moved["canUndo"], true);
        ok(&session.handle_request(&json!({"op":"undo"})));
        let restored = ok(&session.handle_request(&json!({"op":"snapshot"})));
        assert_eq!(restored["model"], before["model"]);
        ok(&session.handle_request(&json!({
            "op":"rig.drag_bone","root":rig_id,"bone":child,"rotation":[0.0,0.4,0.0]
        })));
        let rotated = ok(&session.handle_request(&json!({"op":"snapshot"})));
        assert_ne!(rotated["previewDeformation"], before["previewDeformation"]);
        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"})))["model"], before["model"]);
        let foreign_rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Other"})));
        let foreign = bone(&mut session, foreign_rig["created"][0]["uuid"].as_u64().unwrap(), "Foreign", [0.0, 0.0, 0.0], [0.0, 10.0, 0.0]);
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.drag_bone","root":rig_id,"bone":foreign,"translation":[1.0,0.0,0.0]
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("not in this rig"), "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
    }

    #[test]
    fn driven_drag_writes_one_key_and_split_channels_fail() {
        let mut session = crate::Session::new();
        let rig = ok(&session.handle_request(&json!({"op":"node.create","className":"DepthRigRoot","name":"Rig"})));
        let rig_id = rig["created"][0]["uuid"].as_u64().unwrap();
        let created = ok(&session.handle_request(&json!({
            "op":"node.create","className":"DepthBone","name":"Child","parents":[rig_id],
            "restHead":[0.0,0.0,0.0],"restTail":[0.0,10.0,0.0]
        })));
        let child = created["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({"op":"node.transform","id":child,"translation":[10.0,0.0,0.0]})));
        let grid = ok(&session.handle_request(&json!({"op":"node.create","className":"GridDeformer","name":"Grid"})));
        let grid_id = grid["created"][0]["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"deformer.define_grid","nodes":[grid_id],"axisX":[0.0,10.0],"axisY":[0.0,10.0]
        })));
        ok(&session.handle_request(&json!({"op":"rig.add_source","root":rig_id,"target":grid_id,"bone":child})));
        let parameter = ok(&session.handle_request(&json!({
            "op":"parameter.create","dimensions":1,"name":"Pose","min":[-1.0,0.0],"max":[1.0,1.0]
        })));
        let parameter_id = parameter["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[child],"key":[0,0],
            "translation":[5.0,5.0],"translation_z":1.0
        })));
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[child],"key":[1,0],
            "translation":[0.0,0.0],"translation_z":0.0,
            "rotation_x":0.0,"rotation_y":0.0,"rotation_degrees":0.0
        })));
        let other_key = [
            channel_at(&mut session, parameter_id, child, "transform.t.x", 0),
            channel_at(&mut session, parameter_id, child, "transform.t.y", 0),
            channel_at(&mut session, parameter_id, child, "transform.t.z", 0),
        ];
        let before_deform = deform_cell(&mut session, parameter_id, grid_id);
        let before = ok(&session.handle_request(&json!({"op":"snapshot"})));
        ok(&session.handle_request(&json!({
            "op":"rig.drag_bone","root":rig_id,"bone":child,"translation":[12.0,4.0,1.0]
        })));
        let node = ok(&session.handle_request(&json!({"op":"node.get","id":child})));
        assert_eq!(node["transform"]["trans"], json!([10.0, 0.0, 0.0]));
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.x", 1) - 2.0).abs() < 1e-4);
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.y", 1) - 4.0).abs() < 1e-4);
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.z", 1) - 1.0).abs() < 1e-4);
        let picked = ok(&session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[12.0,4.0,1.0],"radius":0.5
        })));
        assert_eq!(picked["bone"], child);
        let doubled = ok(&session.handle_request(&json!({
            "op":"rig.pick_bone","root":rig_id,"point":[22.0,4.0,1.0],"radius":0.5
        })));
        assert!(doubled["bone"].is_null());
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.x", 0) - other_key[0]).abs() < 1e-9);
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.y", 0) - other_key[1]).abs() < 1e-9);
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.z", 0) - other_key[2]).abs() < 1e-9);
        assert_ne!(deform_cell(&mut session, parameter_id, grid_id), before_deform);
        ok(&session.handle_request(&json!({"op":"undo"})));
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"})))["model"], before["model"]);

        ok(&session.handle_request(&json!({
            "op":"rig.drag_bone","root":rig_id,"bone":child,"rotation":[0.0,0.25,0.0]
        })));
        let rotated = ok(&session.handle_request(&json!({"op":"node.get","id":child})));
        assert_eq!(rotated["transform"]["rot"], json!([0.0, 0.0, 0.0]));
        assert!((channel_at(&mut session, parameter_id, child, "transform.r.y", 1) - 0.25).abs() < 1e-4);
        assert!((channel_at(&mut session, parameter_id, child, "transform.r.x", 1)).abs() < 1e-9);
        ok(&session.handle_request(&json!({"op":"undo"})));

        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":parameter_id,"nodes":[child],"key":[1,0],
            "translation":[0.0,4.0],"translation_z":1.0
        })));
        ok(&session.handle_request(&json!({
            "op":"rig.drag_bone","root":rig_id,"bone":child,"component":"translationX","value":22.0
        })));
        assert_eq!(
            ok(&session.handle_request(&json!({"op":"node.get","id":child})))["transform"]["trans"],
            json!([10.0, 0.0, 0.0])
        );
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.x", 1) - 12.0).abs() < 1e-4);
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.y", 1) - 4.0).abs() < 1e-4);
        assert!((channel_at(&mut session, parameter_id, child, "transform.t.z", 1) - 1.0).abs() < 1e-4);
        ok(&session.handle_request(&json!({"op":"undo"})));

        let split = ok(&session.handle_request(&json!({
            "op":"parameter.create","dimensions":1,"name":"Split","min":[-1.0,0.0],"max":[1.0,1.0]
        })));
        let split_id = split["uuid"].as_u64().unwrap();
        ok(&session.handle_request(&json!({
            "op":"binding.set_trs","parameter":split_id,"nodes":[child],"key":[1,0],"translation_z":0.0
        })));
        let held = ok(&session.handle_request(&json!({"op":"snapshot"})));
        let failed = session.handle_request(&json!({
            "op":"rig.drag_bone","root":rig_id,"bone":child,"translation":[3.0,0.0,0.0]
        }));
        assert_eq!(failed["succeeded"], false, "{failed}");
        assert!(failed["message"].as_str().unwrap().contains("no single translation parameter"), "{failed}");
        assert_eq!(ok(&session.handle_request(&json!({"op":"snapshot"}))), held);
    }
}
